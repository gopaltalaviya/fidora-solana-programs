/**
 * Shared helpers for the scripts: cluster selection (localnet and devnet
 * only — mainnet is refused), keys, PDAs, the Anchor program client.
 */
import {existsSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import anchor from '@anchor-lang/core';
import {Connection, Keypair, LAMPORTS_PER_SOL, PublicKey} from '@solana/web3.js';

export const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
export const IDL = JSON.parse(readFileSync(join(ROOT, 'idl', 'fidora.json'), 'utf8'));
export const PROGRAM_ID = new PublicKey(IDL.address);
export const {BN} = anchor;

const CLUSTERS = {
  localnet: {rpc: 'http://127.0.0.1:8899', explorer: null},
  devnet: {rpc: 'https://api.devnet.solana.com', explorer: 'https://explorer.solana.com'},
};

export function cluster(name, env = process.env) {
  const c = CLUSTERS[name];
  if (!c) throw new Error(`--cluster must be localnet or devnet (got ${name}); mainnet is never used by these scripts`);
  const rpc = name === 'devnet' ? (env.RPC_URL_DEVNET || c.rpc) : (env.RPC_URL_LOCALNET || c.rpc);
  if (/mainnet/i.test(rpc)) throw new Error(`refusing ${rpc}: mainnet`);
  const connection = name === 'localnet'
    ? new Connection(rpc, 'confirmed')
    : new Connection(rpc, {commitment: 'confirmed', fetch: patientFetch, disableRetryOnRateLimit: true});
  return {name, rpc, connection, explorer: c.explorer};
}

/**
 * The public devnet endpoint answers bursts with 429. web3.js's own retry
 * gives up after ~15 s, which a 23-check run outlasts; this one waits up to
 * ~4 minutes per request (1, 2, 4 … capped at 30 s, plus jitter), and only on
 * 429: a dropped connection (a fetch TypeError) is retried the same way; every other status and error is returned unchanged.
 */
export async function patientFetch(url, init) {
  for (let attempt = 0; ; attempt++) {
    let res;
    try {
      res = await fetch(url, init);
    } catch (e) {
      // A dropped connection (undici: "fetch failed") under load: retried too.
      if (attempt >= 12 || !(e instanceof TypeError)) throw e;
      await new Promise((r) => setTimeout(r, Math.min(30_000, 1000 * 2 ** attempt) + Math.random() * 500));
      continue;
    }
    if (res.status !== 429 || attempt >= 12) return res;
    await new Promise((r) => setTimeout(r, Math.min(30_000, 1000 * 2 ** attempt) + Math.random() * 500));
  }
}

export const arg = (n) => (process.argv.includes(`--${n}`) ? process.argv[process.argv.indexOf(`--${n}`) + 1] : undefined);
export const flag = (n) => process.argv.includes(`--${n}`);

export const readKeypair = (path) => Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, 'utf8'))));
export function writeKeypair(path, kp) {
  mkdirSync(dirname(path), {recursive: true});
  writeFileSync(path, JSON.stringify(Array.from(kp.secretKey)), {mode: 0o600});
}

/**
 * The account that pays and owns the deployment. Localnet: a key kept in
 * .localnet/ (gitignored), created and airdropped on first use. Devnet:
 * FIDORA_DEPLOYER_KEYPAIR names a keypair file (never in the repo).
 */
export async function deployer(c, env = process.env) {
  if (c.name === 'localnet') {
    const p = join(ROOT, '.localnet', 'deployer-keypair.json');
    if (!existsSync(p)) writeKeypair(p, Keypair.generate());
    const kp = readKeypair(p);
    if ((await c.connection.getBalance(kp.publicKey)) < 50 * LAMPORTS_PER_SOL) await airdrop(c, kp.publicKey, 100);
    return kp;
  }
  if (!env.FIDORA_DEPLOYER_KEYPAIR) throw new Error('FIDORA_DEPLOYER_KEYPAIR must name the devnet deployer keypair file (in .env)');
  return readKeypair(env.FIDORA_DEPLOYER_KEYPAIR);
}

export async function airdrop(c, to, sol) {
  const sig = await c.connection.requestAirdrop(to, sol * LAMPORTS_PER_SOL);
  await c.connection.confirmTransaction(sig, 'confirmed');
}

export function program(c, payer) {
  const wallet = {
    publicKey: payer.publicKey,
    payer,
    signTransaction: async (tx) => {
      if ('version' in tx) tx.sign([payer]);
      else tx.partialSign(payer);
      return tx;
    },
    signAllTransactions: async (txs) => {
      for (const tx of txs) {
        if ('version' in tx) tx.sign([payer]);
        else tx.partialSign(payer);
      }
      return txs;
    },
  };
  const provider = new anchor.AnchorProvider(c.connection, wallet, {commitment: 'confirmed', preflightCommitment: 'confirmed'});
  return new anchor.Program(IDL, provider);
}

const u64le = (n) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(BigInt(n));
  return b;
};
export const pda = {
  config: () => PublicKey.findProgramAddressSync([Buffer.from('config')], PROGRAM_ID)[0],
  vault: () => PublicKey.findProgramAddressSync([Buffer.from('vault')], PROGRAM_ID)[0],
  agent: (id) => PublicKey.findProgramAddressSync([Buffer.from('agent'), u64le(id)], PROGRAM_ID)[0],
  job: (id) => PublicKey.findProgramAddressSync([Buffer.from('job'), u64le(id)], PROGRAM_ID)[0],
  wallet: (pk) => PublicKey.findProgramAddressSync([Buffer.from('wallet'), new PublicKey(pk).toBuffer()], PROGRAM_ID)[0],
};

/** sha256 as the 32-byte array the program stores. */
export const sha = (s) => Array.from(createHash('sha256').update(s).digest());

export function params(slug) {
  const on = JSON.parse(readFileSync(join(ROOT, 'config', `params.${slug}.json`), 'utf8')).onchain;
  const bn = (v) => new BN(String(v));
  return {
    protocolFeeBps: on.protocol_fee_bps,
    minFee: bn(on.min_fee),
    minJobAmount: bn(on.min_job_amount),
    maxJobAmount: bn(on.max_job_amount),
    fastPathMax: bn(on.fast_path_max),
    reviewWindow: bn(on.review_window),
    maxAcceptWindow: bn(on.max_accept_window),
    maxWorkWindow: bn(on.max_work_window),
    disputeTimeout: bn(on.dispute_timeout),
    bondBps: on.bond_bps,
    minBond: bn(on.min_bond),
    disputeBondBps: on.dispute_bond_bps,
    minDisputeBond: bn(on.min_dispute_bond),
    minStake: bn(on.min_stake),
    withdrawDelay: bn(on.withdraw_delay),
    defaultPerTaskCap: bn(on.default_per_task_cap),
    defaultDailyCap: bn(on.default_daily_cap),
  };
}

export const deploymentFile = (slug) => join(ROOT, 'deployments', `${slug}.json`);
export const readDeployment = (slug) => JSON.parse(readFileSync(deploymentFile(slug), 'utf8'));
