// Client for the m0d relay daemon: framing, requests, and a Viewer that applies one
// attachment's ordered event log to an @xterm/headless instance and acks what it applied.
import net from 'node:net';
import { spawn } from 'node:child_process';
import { EventEmitter } from 'node:events';
import { newTerm, write } from './state.mjs';

const T_JSON = 1, T_DATA = 2, T_INPUT = 3;
export const M0D = process.env.M0D || '/Users/jcarreira/projects/squashmerge/combine-targets/m0-spike/release/m0d';
export const DAEMON_SOCK = process.env.M0D_SOCK || '/tmp/cm0/m0d.sock';

export function sshBridge(sock = DAEMON_SOCK, host = process.env.M0_SSH_HOST || 'localhost') {
  const m0 = process.env.M0 || '/tmp/cm0';
  return ['ssh', '-T', '-o', 'BatchMode=yes', host, 'env', '-u', 'HC_TERMINAL_ID', '-u', 'HC_TERMINAL_SOCKET', '-u', 'HC_SCOPE',
    'HC_SESSION=m0', `XDG_STATE_HOME=${m0}/state`, `XDG_CONFIG_HOME=${m0}/config`, `XDG_RUNTIME_DIR=${m0}/run`,
    M0D, 'connect', '--listen', sock];
}

function frame(type, payload) {
  const b = Buffer.allocUnsafe(5 + payload.length);
  b.writeUInt32BE(payload.length + 1, 0);
  b[4] = type;
  payload.copy(b, 5);
  return b;
}

export class DaemonConn extends EventEmitter {
  // via: 'socket' (direct Unix socket) or 'bridge' (spawn `m0d connect`, i.e. the ssh path minus ssh)
  // via 'ssh': the real path, `ssh -T -o BatchMode=yes <host> <abs m0d> connect`, with the isolation
  // env on the remote command so nothing over there can resolve the default core.
  static connect({ via = process.env.M0_VIA || 'socket', sock = DAEMON_SOCK, bridgeCmd } = {}) {
    if (via === 'ssh' && !bridgeCmd) bridgeCmd = sshBridge(sock);
    return new Promise((resolve, reject) => {
      const c = new DaemonConn();
      let rd, wr;
      if (via === 'bridge' || via === 'ssh') {
        const [cmd, ...args] = bridgeCmd || [M0D, 'connect', '--listen', sock];
        c.child = spawn(cmd, args, { stdio: ['pipe', 'pipe', 'inherit'] });
        rd = c.child.stdout; wr = c.child.stdin;
        c.child.on('exit', () => c._closed());
        wr.on('error', () => {});
      } else {
        const s = net.createConnection(sock);
        s.on('error', (e) => { if (!c.hello) reject(e); c._closed(); });
        s.on('close', () => c._closed());
        rd = s; wr = s; c.sock = s;
      }
      c.wr = wr;
      c.buf = Buffer.alloc(0);
      c.reqs = new Map();
      c.nextReq = 1;
      rd.on('data', (d) => c._onData(d));
      c.once('hello', (m) => (m.op === 'hello' ? resolve(c) : reject(new Error(JSON.stringify(m)))));
      c.sendJson({ op: 'hello', version: 1 });
    });
  }
  _closed() { if (!this.closed) { this.closed = true; this.emit('close'); } }
  close() {
    if (this.sock) this.sock.destroy();
    if (this.child) this.child.kill('SIGTERM');
    this._closed();
  }
  // Stop / restart reading from the daemon (a slow client), whatever the transport.
  pauseReading() { (this.sock || this.child.stdout).pause(); }
  resumeReading() { (this.sock || this.child.stdout).resume(); }
  sendJson(o) { if (!this.closed) this.wr.write(frame(T_JSON, Buffer.from(JSON.stringify(o)))); }
  sendInput(att, bytes) {
    const p = Buffer.allocUnsafe(4 + bytes.length);
    p.writeUInt32BE(att, 0);
    Buffer.from(bytes).copy(p, 4);
    if (!this.closed) this.wr.write(frame(T_INPUT, p));
  }
  // `sync` runs as soon as the reply is parsed, before any later frame in the same chunk.
  request(o, sync) {
    const req = this.nextReq++;
    return new Promise((res) => { this.reqs.set(req, (m) => { if (sync) sync(m); res(m); }); this.sendJson({ ...o, req }); });
  }
  _onData(d) {
    this.buf = this.buf.length ? Buffer.concat([this.buf, d]) : d;
    while (this.buf.length >= 5) {
      const n = this.buf.readUInt32BE(0);
      if (this.buf.length < 4 + n) break;
      const t = this.buf[4];
      const p = this.buf.subarray(5, 4 + n);
      this.buf = this.buf.subarray(4 + n);
      if (t === T_JSON) {
        const m = JSON.parse(p.toString());
        if (!this.hello) { this.hello = m; this.emit('hello', m); continue; }
        if (m.req !== undefined && this.reqs.has(m.req)) { const r = this.reqs.get(m.req); this.reqs.delete(m.req); r(m); continue; }
        this.emit('json', m);
      } else if (t === T_DATA) {
        this.emit('event', { att: p.readUInt32BE(0), epoch: p.readUInt32BE(4), seq: Number(p.readBigUInt64BE(8)), kind: 'data', bytes: Buffer.from(p.subarray(16)) });
      }
    }
  }
}

// One panel: an xterm instance fed by one attachment's ordered log.
export class Viewer extends EventEmitter {
  constructor(conn, opts) {
    super();
    this.opts = { mode: 'active', resize_notifications: true, ...opts };
    this.term = null;
    this.cursor = null; // last applied {att, epoch, seq}: belongs to this xterm instance
    this.queue = Promise.resolve();
    this.applied = 0;
    this.dataBytes = 0;
    this.log = []; // optional raw record of applied events
    this.record = !!opts.record;
    this.ackEvery = opts.ackEvery ?? 1;
    this.setConn(conn);
  }
  setConn(conn) {
    this.conn = conn;
    this._onEv = (ev) => this._event(ev);
    this._onJson = (m) => { if (m.att === this.att && (m.op === 'ev')) this._event(m); else if (m.att === this.att && m.op === 'gap') this.emit('gap', m); };
    conn.on('event', this._onEv);
    conn.on('json', this._onJson);
  }
  detachConn() {
    this.conn.off('event', this._onEv);
    this.conn.off('json', this._onJson);
  }
  async attach() {
    const { terminal, cols, rows, display, mode, history, observe, resize_notifications } = this.opts;
    const r = await this.conn.request({ op: 'attach', terminal, cols, rows, display, mode, history, observe, resize_notifications },
      (m) => { if (m.op === 'attached') this.att = m.att; });
    if (r.op !== 'attached') throw new Error('attach failed: ' + JSON.stringify(r));
    this.att = r.att;
    this.info = r.info;
    return r;
  }
  // Resume on a (new) connection from this xterm's own last-applied cursor.
  async resume(conn) {
    this.detachConn();
    // Apply what already arrived on the old connection first: the cursor must be the
    // last event applied to this xterm, and acks for those events may already be out.
    await this.queue;
    this.setConn(conn);
    const r = await conn.request({ op: 'attach', resume: this.cursor }, (m) => { if (m.op === 'attached') this.att = m.att; });
    return r; // 'attached' (resumed) or 'gap'
  }
  _event(ev) {
    if (ev.att !== this.att) return;
    this.queue = this.queue.then(() => this._apply(ev));
  }
  async _apply(ev) {
    if (ev.kind === 'attached') {
      // New epoch: a fresh first paint follows; discard parser and screen state.
      if (this.term) this.term.dispose();
      this.term = newTerm(ev.cols || this.opts.cols, ev.rows || this.opts.rows);
      this.epochInfo = ev;
      this.emit('epoch', ev);
    } else if (ev.kind === 'data') {
      await write(this.term, ev.bytes);
      this.dataBytes += ev.bytes.length;
      if (this.record) this.log.push({ seq: ev.seq, kind: 'data', bytes: ev.bytes });
    } else if (ev.kind === 'resized') {
      this.term.resize(ev.cols, ev.rows);
      if (this.record) this.log.push({ seq: ev.seq, kind: 'resized', cols: ev.cols, rows: ev.rows });
      this.emit('resized', ev);
    } else {
      if (this.record) this.log.push({ seq: ev.seq, kind: ev.kind });
      this.emit(ev.kind, ev);
    }
    this.cursor = { att: ev.att, epoch: ev.epoch, seq: ev.seq };
    this.applied++;
    if (this.applied % this.ackEvery === 0 || ev.kind !== 'data') this.ack();
    this.emit('applied', ev);
  }
  ack() { if (this.cursor) this.conn.sendJson({ op: 'ack', ...this.cursor }); }
  input(bytes) { this.conn.sendInput(this.att, Buffer.from(bytes)); }
  resize(cols, rows) { this.conn.sendJson({ op: 'resize', att: this.att, cols, rows }); }
  focus(f = true) { this.conn.sendJson({ op: 'focus', att: this.att, focused: f }); }
  detach() { this.conn.sendJson({ op: 'detach', att: this.att }); this.detachConn(); }
  // Resolve once no event has been applied for `ms`.
  quiet(ms = 400, max = 30000) {
    return new Promise((res) => {
      let t; const start = Date.now();
      const arm = () => { clearTimeout(t); t = setTimeout(done, ms); };
      const done = () => { this.off('applied', arm); this.queue.then(res); };
      this.on('applied', arm); arm();
      setTimeout(() => { clearTimeout(t); done(); }, max);
    });
  }
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
