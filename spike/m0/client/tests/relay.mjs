// M0 relay-contract tests through m0d. usage: node tests/relay.mjs [test ...]
// tests: replay overflow resize two backpressure gap hcattach   (default: all)
import fs from 'node:fs';
import { spawn } from 'node:child_process';
import { DaemonConn, Viewer, sleep } from '../lib/m0client.mjs';
import { newTerm, write, snapshot, diff } from '../lib/state.mjs';
import * as E from '../lib/env.mjs';

const FIX = `${E.SPIKE}/fixtures`;
const want = process.argv.slice(2);
const on = (t) => !want.length || want.includes(t);
const results = {};
const BENIGN = [/^tabStops:/, /\.wrapped:/];
const real = (d) => d.filter((x) => !BENIGN.some((re) => re.test(x)));

function dataAfterPaint(v) {
  // Bytes of every data event after the first one (the first paint), in applied order.
  const d = v.log.filter((e) => e.kind === 'data');
  return Buffer.concat(d.slice(1).map((e) => e.bytes));
}
function checkLines(buf, expectN) {
  const re = /L(\d{10}) /g; const s = buf.toString('latin1');
  let m, prev = -1, jumps = [], dups = 0, count = 0;
  while ((m = re.exec(s))) {
    const n = Number(m[1]); count++;
    if (n <= prev) dups++;
    else if (n !== prev + 1) jumps.push(`${prev}->${n}`);
    prev = n;
  }
  return { count, last: prev, jumps: jumps.slice(0, 5), jumpCount: jumps.length, dups, expected: expectN, repaints: s.split('\x18\x1b[!p').length - 1, done: s.includes('DONE') };
}
function seqCheck(v) {
  const seqs = v.log.map((e) => e.seq);
  let gaps = 0, dups = 0;
  for (let i = 1; i < seqs.length; i++) { if (seqs[i] <= seqs[i - 1]) dups++; else if (seqs[i] !== seqs[i - 1] + 1) gaps++; }
  return { events: seqs.length, seqGaps: gaps, seqDups: dups };
}
async function stats(conn) { return (await conn.request({ op: 'stats' })).atts; }

let daemon = await E.startDaemon();

// ---- replay: drop the connection during heavy output, inside the replay window.
if (on('replay')) {
  E.run('fl-replay', ['python3', `${FIX}/flood.py`, '3']);
  await sleep(500);
  let c = await DaemonConn.connect();
  const v = new Viewer(c, { terminal: 'fl-replay', cols: 80, rows: 24, record: true, ackEvery: 8 });
  await v.attach(); await v.quiet(300);
  v.input('g');
  const drops = [];
  for (let i = 0; i < 10; i++) {
    await sleep(15);
    await v.queue; const cur = v.cursor; c.close(); drops.push(cur.seq);
    await sleep(60);
    c = await DaemonConn.connect();
    const r = await v.resume(c);
    if (r.op !== 'attached') { drops.push('GAP ' + JSON.stringify(r)); break; }
  }
  await v.quiet(1500);
  const lines = checkLines(dataAfterPaint(v), 3 * 1024 * 1024 / 64);
  const sq = seqCheck(v);
  const control = new Viewer(c, { terminal: 'fl-replay', cols: 80, rows: 24 });
  await control.attach(); await control.quiet(500);
  const d = real(diff(snapshot(control.term), snapshot(v.term)));
  results.replay = { pass: lines.count === lines.expected && !lines.jumpCount && !lines.dups && !lines.repaints && !sq.seqGaps && !sq.seqDups && !d.length, drops, lines, ...sq, finalScreenVsFreshAttach: d };
  console.log('replay', JSON.stringify(results.replay));
  v.detach(); control.detach(); c.close(); E.kill('fl-replay');
}

// ---- overflow: stay away longer than the replay window: herdr-core resyncs in-band.
if (on('overflow')) {
  E.run('fl-over', ['python3', `${FIX}/flood.py`, '16']);
  await sleep(500);
  let c = await DaemonConn.connect();
  const v = new Viewer(c, { terminal: 'fl-over', cols: 80, rows: 24, record: true, ackEvery: 8 });
  await v.attach(); await v.quiet(300);
  v.input('g'); await sleep(50);
  c.close(); await sleep(4000);
  const c0 = await DaemonConn.connect();
  const st = (await stats(c0)).find((a) => a.att === v.att);
  c = await DaemonConn.connect();
  const r = await v.resume(c);
  await v.quiet(2000);
  const lines = checkLines(dataAfterPaint(v), 16 * 1024 * 1024 / 64);
  const control = new Viewer(c, { terminal: 'fl-over', cols: 80, rows: 24 });
  await control.attach(); await control.quiet(500);
  const d = real(diff(snapshot(control.term), snapshot(v.term)));
  results.overflow = { resume: r.op, daemonLogWhileAway: st, lines, ...seqCheck(v), finalScreenVsFreshAttach: d, pass: r.op === 'attached' && d.length === 0 };
  console.log('overflow', JSON.stringify(results.overflow));
  v.detach(); control.detach(); c.close(); c0.close(); E.kill('fl-over');
}

// ---- resize: drop just before / just after a resize; compare with a client that never dropped.
if (on('resize')) {
  E.run('rz-vim', ['vim', '-u', 'NONE', '-N', '-i', 'NONE', '-n', '+set number', `${FIX}/sample.txt`]);
  await sleep(1000);
  const out = {};
  for (const mode of ['paired', 'active']) {
    for (const when of ['before', 'after']) {
      let cA = await DaemonConn.connect();
      const cB = await DaemonConn.connect();
      const A = new Viewer(cA, { terminal: 'rz-vim', cols: 80, rows: 24, mode, record: true });
      const B = new Viewer(cB, { terminal: 'rz-vim', cols: 80, rows: 24, mode, record: true });
      await A.attach(); await B.attach(); await A.quiet(400); await B.quiet(100);
      const size = when === 'before' ? [100, 30] : [90, 20];
      // In active mode only the resizing client learns the size, so A both resizes and drops.
      const resizer = mode === 'paired' ? B : A;
      if (when === 'before') { cA.close(); await sleep(20); if (mode === 'paired') resizer.resize(...size); }
      else { resizer.resize(...size); await sleep(5); cA.close(); }
      await sleep(600);
      cA = await DaemonConn.connect();
      const r = await A.resume(cA);
      if (mode === 'active' && when === 'before') { A.resize(...size); }
      await A.quiet(800); await B.quiet(200);
      const ref = mode === 'paired' ? B : null;
      let d;
      if (ref) d = diff(snapshot(ref.term), snapshot(A.term));
      else {
        const F = new Viewer(cB, { terminal: 'rz-vim', cols: size[0], rows: size[1] }); await F.attach(); await F.quiet(500);
        d = real(diff(snapshot(F.term), snapshot(A.term))); F.detach();
      }
      const sq = seqCheck(A);
      const resizes = A.log.filter((e) => e.kind === 'resized').map((e) => `${e.cols}x${e.rows}@${e.seq}`);
      out[`${mode}_${when}`] = { resume: r.op, size: A.term ? `${A.term.cols}x${A.term.rows}` : null, resizes, ...sq, diffs: d.slice(0, 8), pass: r.op === 'attached' && d.length === 0 && !sq.seqGaps && !sq.seqDups };
      console.log('resize', mode, when, JSON.stringify(out[`${mode}_${when}`]));
      A.detach(); B.detach(); cA.close(); cB.close();
      // back to 80x24 for the next round
      const cR = await DaemonConn.connect(); const R = new Viewer(cR, { terminal: 'rz-vim', cols: 80, rows: 24 }); await R.attach(); R.focus(); await R.quiet(500); R.detach(); cR.close();
    }
  }
  results.resize = out;
  E.kill('rz-vim');
}

// ---- two attachments, different sizes and display facts.
if (on('two')) {
  E.run('two', ['python3', `${FIX}/qloop.py`]);
  await sleep(500);
  const out = {};
  for (const mode of ['active', 'paired']) {
    const c = await DaemonConn.connect();
    const A = new Viewer(c, { terminal: 'two', cols: 80, rows: 24, mode, record: true, display: { fg: '#ffffff', bg: '#aa0000', cursor: '#ffffff', cell: [8, 16] } });
    const B = new Viewer(c, { terminal: 'two', cols: 120, rows: 40, mode, record: true, display: { fg: '#000000', bg: '#00bb00', cursor: '#000000', cell: [10, 20] } });
    await A.attach(); await A.quiet(300); await B.attach(); await B.quiet(300);
    const text = (v) => Buffer.concat(v.log.filter((e) => e.kind === 'data').map((e) => e.bytes)).toString('latin1');
    A.focus(); await sleep(300); A.input('q'); await sleep(1500);
    B.focus(); await sleep(300); B.input('q'); await sleep(1500);
    A.input('a'); await sleep(300); B.input('b'); await sleep(300);
    await A.quiet(300); await B.quiet(300);
    const replies = [...text(A).matchAll(/REPLY bg=(.*?) size=(.*?)\r\n/g)].map((m) => `bg=${m[1]} size=${m[2]}`);
    const g = E.hc('get', 'two');
    out[mode] = {
      replies_seen_by_A: replies,
      A_term: `${A.term.cols}x${A.term.rows}`, B_term: `${B.term.cols}x${B.term.rows}`, pty: `${g.cols}x${g.rows}`,
      A_resizes: A.log.filter((e) => e.kind === 'resized').map((e) => `${e.cols}x${e.rows}`),
      B_resizes: B.log.filter((e) => e.kind === 'resized').map((e) => `${e.cols}x${e.rows}`),
      input_from_A_seen: text(B).includes('<got 61>'), input_from_B_seen: text(A).includes('<got 62>'),
    };
    console.log('two', mode, JSON.stringify(out[mode]));
    A.detach(); B.detach(); c.close();
    await sleep(300);
  }
  results.two = out;
  E.kill('two');
}

// ---- backpressure: a client that stops reading; daemon memory stays bounded.
if (on('backpressure')) {
  E.run('fl-bp', ['python3', `${FIX}/flood.py`, '50']);
  await sleep(500);
  const c = await DaemonConn.connect();
  const v = new Viewer(c, { terminal: 'fl-bp', cols: 80, rows: 24, record: false, ackEvery: 8 });
  await v.attach(); await v.quiet(300);
  const rss0 = E.rssKb(daemon.pid);
  c.pauseReading();
  v.input('g');
  let peak = 0, maxLog = 0;
  const c0 = await DaemonConn.connect();
  for (let i = 0; i < 40; i++) {
    await sleep(250);
    peak = Math.max(peak, E.rssKb(daemon.pid));
    const st = (await stats(c0)).find((a) => a.att === v.att);
    maxLog = Math.max(maxLog, st?.log_bytes || 0);
  }
  c.resumeReading();
  await v.quiet(3000, 120000);
  const F = new Viewer(c0, { terminal: 'fl-bp', cols: 80, rows: 24 }); await F.attach(); await F.quiet(500);
  const d = real(diff(snapshot(F.term), snapshot(v.term)));
  results.backpressure = { mode: 'ack', rss_kb_before: rss0, rss_kb_peak: peak, max_log_bytes: maxLog, finalScreenVsFreshAttach: d, pass: maxLog <= 4 << 20 && d.length === 0 };
  console.log('backpressure', JSON.stringify(results.backpressure));
  v.detach(); F.detach(); c.close(); c0.close(); E.kill('fl-bp');
}

// ---- gap: eager credit with a small cap: the daemon overflows and answers Gap.
if (on('gap')) {
  await E.stopDaemon(daemon);
  daemon = await E.startDaemon({ credit: 'eager', logCap: 1 << 20 });
  E.run('fl-gap', ['python3', `${FIX}/flood.py`, '8']);
  await sleep(500);
  let c = await DaemonConn.connect();
  const v = new Viewer(c, { terminal: 'fl-gap', cols: 80, rows: 24, record: false, ackEvery: 8 });
  await v.attach(); await v.quiet(300);
  const gaps = [];
  v.on('gap', (m) => gaps.push(m));
  c.pauseReading(); v.input('g'); await sleep(3000);
  const c0 = await DaemonConn.connect();
  const st = (await stats(c0)).find((a) => a.att === v.att);
  c.resumeReading(); await sleep(1500);
  // Also: resume a dropped client from a cursor that has been trimmed.
  c.close(); await sleep(200);
  c = await DaemonConn.connect();
  const r = await v.resume(c);
  // On Gap: discard the xterm (new instance) and attach fresh.
  const F = new Viewer(c, { terminal: 'fl-gap', cols: 80, rows: 24 }); await F.attach(); await F.quiet(800);
  results.gap = { mode: 'eager, cap 1 MiB', stats_while_paused: st, live_gap_events: gaps, resume_reply: r, fresh_after_gap: F.info, pass: gaps.length > 0 && r.op === 'gap' };
  console.log('gap', JSON.stringify(results.gap));
  F.detach(); c.close(); c0.close(); E.kill('fl-gap');
  await E.stopDaemon(daemon);
  daemon = await E.startDaemon();
}

// ---- hc attach alongside a Combine attachment.
if (on('hcattach')) {
  E.run('hca', ['python3', `${FIX}/qloop.py`]);
  await sleep(500);
  const c = await DaemonConn.connect();
  const A = new Viewer(c, { terminal: 'hca', cols: 80, rows: 24, mode: 'paired', record: true });
  await A.attach(); await A.quiet(300);
  const env = { ...process.env, HC_SESSION: 'm0', XDG_STATE_HOME: `${E.M0}/state`, XDG_CONFIG_HOME: `${E.M0}/config`, XDG_RUNTIME_DIR: `${E.M0}/run`, HC_SOCKET: E.CORE_SOCK };
  for (const k of ['HC_TERMINAL_ID', 'HC_TERMINAL_SOCKET', 'HC_SCOPE']) delete env[k];
  const outFile = `${E.M0}/hcattach.out`;
  // pty_attach.py sets its pty to 80x24 then resizes to 100x30, types "h", detaches.
  const p = spawn('python3', ['-I', `${E.SPIKE}/tools/pty_attach.py`, 'hca', outFile, 'size:100x30', 'send:h', 'sleep:1', 'detach'], { env, stdio: 'ignore' });
  await new Promise((res) => p.on('exit', res));
  A.input('a'); await sleep(500); await A.quiet(300);
  const text = Buffer.concat(A.log.filter((e) => e.kind === 'data').map((e) => e.bytes)).toString('latin1');
  const hcOut = fs.readFileSync(outFile).toString('latin1');
  results.hcattach = {
    combine_saw_hc_attach_input: text.includes('<got 68>'), hc_attach_saw_output: hcOut.includes('qloop ready'),
    combine_resizes: A.log.filter((e) => e.kind === 'resized').map((e) => `${e.cols}x${e.rows}`),
    combine_input_after: text.includes('<got 61>'), final_pty: (() => { const g = E.hc('get', 'hca'); return `${g.cols}x${g.rows}`; })(),
  };
  results.hcattach.pass = results.hcattach.combine_saw_hc_attach_input && results.hcattach.hc_attach_saw_output && results.hcattach.combine_input_after;
  console.log('hcattach', JSON.stringify(results.hcattach));
  A.detach(); c.close(); E.kill('hca');
}

await E.stopDaemon(daemon);
fs.mkdirSync(`${E.SPIKE}/results`, { recursive: true });
const file = `${E.SPIKE}/results/relay${want.length ? '-' + want.join('-') : ''}.json`;
fs.writeFileSync(file, JSON.stringify(results, null, 1));
console.log('wrote', file);
process.exit(0);
