// Isolated herdr-core (never the user's default core) and m0d process helpers.
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import { M0D, DAEMON_SOCK, sleep } from './m0client.mjs';

export const M0 = process.env.M0 || '/tmp/cm0';
export const CORE_SOCK = `${M0}/run/herdr-core/m0.sock`;
export const DEFAULT_CORE_ID = '4bcff4a58f4d97d505396e5a43a807e6';
export const SPIKE = new URL('../..', import.meta.url).pathname.replace(/\/$/, '');

const env = { ...process.env, HC_SESSION: 'm0', XDG_STATE_HOME: `${M0}/state`, XDG_CONFIG_HOME: `${M0}/config`, XDG_RUNTIME_DIR: `${M0}/run` };
for (const k of ['HC_TERMINAL_ID', 'HC_TERMINAL_SOCKET', 'HC_SCOPE', 'HC_SOCKET']) delete env[k];

export function hc(...args) {
  const i = args.indexOf('--');
  const full = i < 0 ? [...args, '--json'] : [...args.slice(0, i), '--json', ...args.slice(i)];
  const out = execFileSync('hc', full, { env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  try { return JSON.parse(out); } catch { return out; }
}
export function hcText(...args) {
  return execFileSync('hc', args, { env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

export function assertIsolated() {
  const s = hc('server', 'status');
  if (s.core.id === DEFAULT_CORE_ID) throw new Error('REFUSING: isolated hc resolves to the default core');
  return s.core.id;
}

export function run(name, argv, { cols = 80, rows = 24 } = {}) {
  assertIsolated();
  try { hc('kill', name); } catch {}
  try { hc('rm', name); } catch {}
  // hc run has no --size flag; the isolated core's config sets headless = "80x24".
  const r = hc('run', '--bg', '--no-parent', '--name', name, '--', ...argv);
  return r;
}

export function kill(name) {
  try { hc('kill', name); } catch {}
  try { hc('rm', name); } catch {}
}

export function rssKb(pid) {
  try { return Number(execFileSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' }).trim()); } catch { return null; }
}

export async function startDaemon({ credit = 'ack', logCap, graceMs, quiet = true } = {}) {
  assertIsolated();
  // A daemon leaked by an earlier, interrupted run would keep running unseen once its socket is unlinked.
  try { execFileSync('pkill', ['-TERM', '-f', `m0d serve --listen ${DAEMON_SOCK} `]); await sleep(200); } catch {}
  try { fs.unlinkSync(DAEMON_SOCK); } catch {}
  const args = ['serve', '--listen', DAEMON_SOCK, '--core', CORE_SOCK, '--data', `${M0}/m0d-data`, '--credit', credit];
  if (logCap) args.push('--log-cap', String(logCap));
  if (graceMs) args.push('--grace-ms', String(graceMs));
  const child = spawn(M0D, args, { stdio: ['ignore', 'ignore', quiet ? 'ignore' : 'inherit'] });
  for (let i = 0; i < 100 && !fs.existsSync(DAEMON_SOCK); i++) await sleep(50);
  await sleep(100);
  return child;
}

export function stopDaemon(child) {
  return new Promise((res) => { child.once('exit', res); child.kill('SIGTERM'); });
}

export function readTrace(path) {
  return fs.existsSync(path) ? fs.readFileSync(path) : Buffer.alloc(0);
}
