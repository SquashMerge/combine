// M0 measurements. Run whole through heavy.sh (timing must not share the CPU with other builds).
// usage: node tests/measure.mjs latency [N]   keystroke echo RTT: herdr-core direct, daemon socket, daemon via `m0d connect`
//        node tests/measure.mjs burst [MB]    MB output burst through daemon + bridge: throughput, memory, loss
import fs from 'node:fs';
import net from 'node:net';
import { DaemonConn, Viewer, sleep } from '../lib/m0client.mjs';
import * as E from '../lib/env.mjs';

const FIX = `${E.SPIKE}/fixtures`;
const what = process.argv[2];
const pct = (a, p) => a[Math.min(a.length - 1, Math.floor(a.length * p))];
const summary = (a) => { const s = [...a].sort((x, y) => x - y); return { n: s.length, p50: +pct(s, 0.5).toFixed(3), p95: +pct(s, 0.95).toFixed(3), p99: +pct(s, 0.99).toFixed(3), max: +s[s.length - 1].toFixed(3), mean: +(s.reduce((x, y) => x + y, 0) / s.length).toFixed(3) }; };
const now = () => Number(process.hrtime.bigint()) / 1e6;

// Minimal herdr-core framed client (baseline without the daemon).
async function coreDirect(terminal) {
  const s = net.createConnection(E.CORE_SOCK);
  let buf = Buffer.alloc(0), helloDone = false, ch = null;
  const listeners = [];
  s.on('data', (d) => {
    buf = Buffer.concat([buf, d]);
    if (!helloDone) { const i = buf.indexOf(10); if (i < 0) return; buf = buf.subarray(i + 1); helloDone = true; }
    while (buf.length >= 9 && buf.length >= 9 + buf.readUInt32LE(5)) {
      const c = buf.readUInt32LE(0), k = buf[4], n = buf.readUInt32LE(5), p = buf.subarray(9, 9 + n);
      buf = buf.subarray(9 + n);
      if (c === 0) { const m = JSON.parse(p); if (m.id === 1) ch = m.result.channel; }
      else if (c === ch && k === 0) { send(ch, 1, Buffer.from(Uint32Array.of(p.length).buffer)); for (const l of listeners) l(p); }
    }
  });
  const send = (c, k, p) => { const h = Buffer.alloc(9); h.writeUInt32LE(c, 0); h[4] = k; h.writeUInt32LE(p.length, 5); s.write(Buffer.concat([h, p])); };
  s.write('{"jsonrpc":"2.0","id":0,"method":"hello","params":{"framed":true,"generation":1}}\n');
  while (!helloDone) await sleep(5);
  send(0, 0, Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'terminal.attach', params: { terminal, cols: 80, rows: 24 } })));
  while (ch === null) await sleep(5);
  return { onData: (f) => listeners.push(f), input: (b) => send(ch, 0, Buffer.from(b)), close: () => s.destroy() };
}

async function rtt(io, n) {
  const out = [];
  let waiting = null;
  io.onData((d) => { if (waiting && d.includes(waiting.ch)) { const w = waiting; waiting = null; w.res(now() - w.t0); } });
  await sleep(500);
  for (let i = 0; i < n; i++) {
    const ch = String.fromCharCode(97 + (i % 26));
    const p = new Promise((res) => { waiting = { ch, t0: now(), res }; });
    io.input(ch);
    const ms = await Promise.race([p, sleep(2000).then(() => -1)]);
    if (ms >= 0) out.push(ms);
    await sleep(3);
  }
  return out;
}

if (what === 'latency') {
  const n = Number(process.argv[3] || 1000);
  E.run('lat', ['python3', `${FIX}/echo.py`]);
  await sleep(500);
  const daemon = await E.startDaemon();
  const res = {};
  const d = await coreDirect('lat');
  res.herdr_core_direct = summary(await rtt(d, n)); d.close();
  for (const via of ['socket', 'bridge', 'ssh']) {
    const c = await DaemonConn.connect({ via });
    const v = new Viewer(c, { terminal: 'lat', cols: 80, rows: 24, ackEvery: 1 });
    const io = { onData: (f) => c.on('event', (ev) => { if (ev.kind === 'data') f(ev.bytes); }), input: (b) => v.input(b) };
    await v.attach(); await v.quiet(300);
    res[`daemon_${via}`] = summary(await rtt(io, n));
    v.detach(); c.close();
  }
  console.log(JSON.stringify(res, null, 1));
  await E.stopDaemon(daemon);
  E.kill('lat');
  fs.mkdirSync(`${E.SPIKE}/results`, { recursive: true });
  fs.writeFileSync(`${E.SPIKE}/results/latency.json`, JSON.stringify(res, null, 1));
}

// From another machine: only the bridge command runs locally; the echo terminal and m0d run on the host.
// M0_BRIDGE='ssh -T -o BatchMode=yes mini /path/to/m0d connect --listen /tmp/cm0/m0d.sock' \
//   node tests/measure.mjs latency-remote <terminal> [N]
if (what === 'latency-remote') {
  const bridgeCmd = (process.env.M0_BRIDGE || '').split(' ').filter(Boolean);
  if (!bridgeCmd.length) throw new Error('set M0_BRIDGE to the ssh command');
  const terminal = process.argv[3] || 'lat';
  const n = Number(process.argv[4] || 500);
  const c = await DaemonConn.connect({ via: 'bridge', bridgeCmd });
  console.log('host', JSON.stringify(c.hello));
  const v = new Viewer(c, { terminal, cols: 80, rows: 24, ackEvery: 1 });
  const io = { onData: (f) => c.on('event', (ev) => { if (ev.kind === 'data') f(ev.bytes); }), input: (b) => v.input(b) };
  await v.attach(); await v.quiet(300);
  console.log(JSON.stringify({ daemon_over_ssh: summary(await rtt(io, n)) }, null, 1));
  v.detach(); c.close();
}

if (what === 'burst') {
  const mb = Number(process.argv[3] || 50);
  const res = {};
  for (const client of ['count', 'xterm', 'ssh-xterm']) {
    E.run('burst', ['python3', `${FIX}/flood.py`, String(mb)]);
    await sleep(500);
    const daemon = await E.startDaemon();
    const corePid = E.hc('server', 'status').pid;
    const c = await DaemonConn.connect({ via: client === 'ssh-xterm' ? 'ssh' : 'bridge' });
    const v = new Viewer(c, { terminal: 'burst', cols: 80, rows: 24, ackEvery: 16 });
    await v.attach(); await v.quiet(300);
    let bytes = 0, lastLine = -1, jumps = 0, dups = 0, repaints = 0, done = false, tail = '';
    const re = /L(\d{10}) /g;
    if (client === 'count') { v.term && (v._apply = async (ev) => { v.cursor = { att: ev.att, epoch: ev.epoch, seq: ev.seq }; if (++v.applied % 16 === 0) v.ack(); v.emit('applied', ev); }); }
    c.on('event', (ev) => {
      if (ev.kind !== 'data') return;
      bytes += ev.bytes.length;
      const s = tail + ev.bytes.toString('latin1');
      repaints += s.split('\x18\x1b[!p').length - 1;
      let m; re.lastIndex = 0;
      while ((m = re.exec(s))) { const n = Number(m[1]); if (n <= lastLine) { if (m.index >= tail.length) dups++; } else { if (n !== lastLine + 1) jumps++; lastLine = n; } }
      if (s.includes('DONE')) done = true;
      tail = s.slice(-16);
    });
    let peakD = 0, peakC = 0;
    const sampler = setInterval(() => { peakD = Math.max(peakD, E.rssKb(daemon.pid) || 0); peakC = Math.max(peakC, E.rssKb(corePid) || 0); }, 100);
    const rssD0 = E.rssKb(daemon.pid), rssC0 = E.rssKb(corePid);
    const t0 = now();
    v.input('g');
    while (!done && now() - t0 < 300000) await sleep(20);
    const secs = (now() - t0) / 1000;
    await v.quiet(500);
    clearInterval(sampler);
    const expected = Math.floor(mb * 1024 * 1024 / 64);
    res[client] = { mb, seconds: +secs.toFixed(2), MBps: +(bytes / 1048576 / secs).toFixed(1), bytes_received: bytes, lines_last: lastLine, lines_expected: expected,
      jumps, dups, core_repaints_in_band: repaints, daemon_rss_kb: { before: rssD0, peak: peakD }, core_rss_kb: { before: rssC0, peak: peakC },
      no_loss: lastLine === expected - 1 && jumps === 0 && dups === 0 };
    console.log(client, JSON.stringify(res[client]));
    v.detach(); c.close();
    await E.stopDaemon(daemon);
    E.kill('burst');
  }
  fs.mkdirSync(`${E.SPIKE}/results`, { recursive: true });
  fs.writeFileSync(`${E.SPIKE}/results/burst.json`, JSON.stringify(res, null, 1));
}
process.exit(0);
