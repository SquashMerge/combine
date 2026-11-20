// M0 go/no-go: does a fresh attach restore the terminal's state from herdr-core's first paint?
// Oracle: a reference @xterm/headless fed the program's whole output since spawn (its trace),
// compared with a fresh xterm fed only what the attachment delivered.
// Cases: (a) terminal started before the daemon, (b) after a daemon restart,
//        (c) after `hc server restart` (the daemon re-attaches in a new epoch; plus a fresh attach).
// usage: node tests/gonogo.mjs [modes|vim|less] [--no-restart]
import fs from 'node:fs';
import { DaemonConn, Viewer, sleep } from '../lib/m0client.mjs';
import { newTerm, write, snapshot, diff, screenText } from '../lib/state.mjs';
import * as E from '../lib/env.mjs';

const fixture = process.argv[2] || 'modes';
const doRestart = !process.argv.includes('--no-restart');
const FIX = `${E.SPIKE}/fixtures`;
const trace = `${E.M0}/trace-${fixture}.bin`;
const name = `gng-${fixture}`;
const DISPLAY = { fg: '#d0d0d0', bg: '#101018', cursor: '#ffcc00', cell: [9, 18] };
const results = { fixture, cases: {} };

function argvFor(f) {
  if (f === 'modes') return ['python3', `${FIX}/modes.py`, trace];
  if (f === 'modes2') return ['python3', `${FIX}/modes2.py`, trace];
  if (f === 'vim') return ['python3', `${FIX}/tee_pty.py`, trace, 'vim', '-u', 'NONE', '-N', '-i', 'NONE', '-n',
    '+set mouse=a', '+set number', '+syntax on', '+12', '+normal 7|', `${FIX}/sample.txt`];
  if (f === 'less') return ['python3', `${FIX}/tee_pty.py`, trace, 'less', '-R', '+15', `${FIX}/sample.txt`];
  throw new Error('unknown fixture ' + f);
}

async function reference() {
  const t = newTerm(80, 24);
  const b = E.readTrace(trace);
  // tee_pty marks size changes in the trace as OSC m0;winch;CxR BEL.
  let i = 0; const mark = Buffer.from('\x1b]m0;winch;');
  while (i < b.length) {
    const j = b.indexOf(mark, i);
    if (j < 0) { await write(t, b.subarray(i)); break; }
    await write(t, b.subarray(i, j));
    const k = b.indexOf(7, j);
    const [c, r] = b.subarray(j + mark.length, k).toString().split('x').map(Number);
    t.resize(c, r);
    i = k + 1;
  }
  return t;
}

async function settle() {
  let last = -1;
  for (let i = 0; i < 60; i++) {
    const n = E.readTrace(trace).length;
    if (n === last && n > 0) return;
    last = n; await sleep(300);
  }
}

async function compare(label, term, extra = {}) {
  const ref = await reference();
  const d = diff(snapshot(ref), snapshot(term));
  results.cases[label] = { pass: d.length === 0, diffs: d, ...extra };
  console.log(`\n[${label}] ${d.length === 0 ? 'IDENTICAL' : d.length + ' difference(s)'}`);
  for (const x of d.slice(0, 40)) console.log('   ' + x);
  return d;
}

function readCheck(label) {
  // terminal.read (hc read) text vs the reference screen text.
  const txt = E.hcText('read', name).replace(/\n$/, '').split('\n').map((l) => l.replace(/\s+$/, ''));
  return reference().then((ref) => {
    const want = screenText(ref).split('\n').map((l) => l.replace(/\s+$/, ''));
    while (want.length && want[want.length - 1] === '') want.pop();
    while (txt.length && txt[txt.length - 1] === '') txt.pop();
    const same = JSON.stringify(want) === JSON.stringify(txt);
    results.cases[label] = { pass: same, read: txt, reference: want };
    console.log(`\n[${label}] terminal.read text ${same ? 'matches' : 'DIFFERS from'} the reference screen`);
    if (!same) for (let i = 0; i < Math.max(want.length, txt.length); i++) if (want[i] !== txt[i]) console.log(`   row ${i}: ref=${JSON.stringify(want[i])} read=${JSON.stringify(txt[i])}`);
  });
}

async function fresh(conn) {
  const v = new Viewer(conn, { terminal: name, cols: 80, rows: 24, display: DISPLAY, record: true });
  await v.attach();
  await v.quiet(600);
  return v;
}

function firstPaint(v) {
  const bytes = Buffer.concat(v.log.filter((e) => e.kind === 'data').map((e) => e.bytes));
  return bytes;
}

E.assertIsolated();
try { fs.unlinkSync(trace); } catch {}
E.kill(name);
E.run(name, argvFor(fixture));
await settle();
await sleep(500);

// (a) terminal started before the daemon existed.
let daemon = await E.startDaemon();
let conn = await DaemonConn.connect();
let v = await fresh(conn);
const paint = firstPaint(v);
fs.writeFileSync(`${E.M0}/paint-${fixture}-a.bin`, paint);
console.log(`first paint: ${paint.length} bytes in ${v.log.length} data event(s); info=${JSON.stringify(v.info)}`);
await compare('a_fresh_attach_before_daemon', v.term, { paint_bytes: paint.length, info: v.info });
await readCheck('a_terminal_read');

// (b) daemon restart: the old cursor is unknown to the new daemon -> Gap -> fresh attach.
const oldCursor = v.cursor;
await E.stopDaemon(daemon);
daemon = await E.startDaemon();
conn = await DaemonConn.connect();
const r = await conn.request({ op: 'attach', resume: oldCursor });
console.log(`\nresume after daemon restart: ${JSON.stringify(r)}`);
results.cases.b_resume_reply = { pass: r.op === 'gap', reply: r };
v = await fresh(conn);
await compare('b_fresh_attach_after_daemon_restart', v.term);

// (c) planned herdr-core restart while attached.
if (doRestart) {
  E.assertIsolated();
  const epochs = [];
  v.on('epoch', (ev) => epochs.push(ev));
  E.hc('server', 'restart');
  const t0 = Date.now();
  while (!epochs.length && Date.now() - t0 < 30000) await sleep(100);
  await v.quiet(800);
  const g = E.hc('get', name);
  console.log(`\nafter server restart: re-attached in ${Date.now() - t0} ms, epoch event ${JSON.stringify(epochs[0])}, screen=${g.screen}`);
  await compare('c_daemon_reattach_after_server_restart', v.term, { epoch_event: epochs[0], screen: g.screen });
  const conn2 = await DaemonConn.connect();
  const v2 = await fresh(conn2);
  fs.writeFileSync(`${E.M0}/paint-${fixture}-c.bin`, firstPaint(v2));
  await compare('c_fresh_attach_after_server_restart', v2.term, { screen: g.screen });
  await readCheck('c_terminal_read');
  v2.detach(); conn2.close();
}

// Live output after the paint: does lost state change what later bytes do?
if (fixture === 'modes') {
  for (const [k, what] of [['p', 'pen'], ['r', 'saved cursor (DECRC)'], ['s', 'scroll region'], ['w', 'autowrap'], ['q', 'leave alt screen -> main screen']]) {
    v.input(k);
    await sleep(300); await v.quiet(400);
    await compare(`live_${k}_${what.replace(/[^a-z]+/gi, '_')}`, v.term);
  }
} else if (fixture === 'modes2') {
  for (const [k, what] of [['g', 'charset'], ['i', 'insert mode'], ['o', 'origin mode'], ['w', 'autowrap off']]) {
    v.input(k);
    await sleep(300); await v.quiet(400);
    await compare(`live_${k}_${what.replace(/[^a-z]+/gi, '_')}`, v.term);
  }
} else if (fixture === 'vim') {
  v.input(':q!\r'); await sleep(500); await v.quiet(500);
  await compare('live_vim_quit_restores_main_screen', v.term);
} else if (fixture === 'less') {
  v.input('q'); await sleep(500); await v.quiet(500);
  await compare('live_less_quit_restores_main_screen', v.term);
}

v.detach(); conn.close();
await E.stopDaemon(daemon);
E.kill(name);
fs.mkdirSync(`${E.SPIKE}/results`, { recursive: true });
fs.writeFileSync(`${E.SPIKE}/results/gonogo-${fixture}.json`, JSON.stringify(results, null, 1));
console.log(`\nsummary: ${Object.entries(results.cases).map(([k, x]) => `${k}=${x.pass ? 'PASS' : 'FAIL'}`).join(' ')}`);
process.exit(0);
