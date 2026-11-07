#!/usr/bin/env node
/**
 * Every flow, live on a real Solana validator, against the deployed program.
 *
 *   node scripts/verify-solana.mjs --cluster localnet
 *   node --env-file=.env scripts/verify-solana.mjs --cluster devnet
 *
 * A mock that agrees with our assumptions proves nothing (project rule). This
 * sends real transactions, signed the way production signs them — owners for
 * owner methods, each agent's separate wallet keypair for job methods — and
 * checks each result in the accounts on chain. Fresh keys each run; on devnet
 * each is saved under .keys/devnet/ before it is funded, and swept back to the
 * deployer at the end (and at the start, for a run that stopped half-way).
 *
 * Checks, in order: registration (register_agent + accept_wallet by the
 * wallet itself), funding and staking, the escrow happy path, direct_pay, a
 * per-task cap refusal, self-dealing and same-owner refusals, the agent key
 * refused at withdraw (only the owner may), the owner's withdraw, a stolen
 * worker key refused locking a bond beyond its caps, a dispute
 * ruled by the arbiter, every exit anyone may call (expire_unaccepted,
 * expire_undelivered, auto_approve, expire_dispute), and invariant I1: the
 * vault's token balance equals the sum of what the program owes.
 */
import {LAMPORTS_PER_SOL, SystemProgram, Transaction} from '@solana/web3.js';
import {TOKEN_PROGRAM_ID, getAccount, getOrCreateAssociatedTokenAccount, mintTo} from '@solana/spl-token';
import {BN, ROOT, arg, cluster, deployer, params as readParams, pda, program, readDeployment, sha} from './lib/fidora.mjs';
import {newKey, sweep} from './lib/throwaway.mjs';

const slug = arg('cluster');
const c = cluster(slug);
const dep = readDeployment(slug);
const root = await deployer(c);
const prog = program(c, root);
const P = readParams(slug);
const mint = new (await import('@solana/web3.js')).PublicKey(dep.accounts.paymentMint);

let passed = 0;
const ok = (what, extra = '') => {
  passed++;
  console.log(`  ✓ ${what}${extra ? `  ${extra}` : ''}`);
};
const link = (sig) => (c.explorer ? `${c.explorer}/tx/${sig}?cluster=${slug}` : sig);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** The call must fail, and the failure must name `reason`. */
async function refused(promise, reason, what) {
  try {
    const sig = await promise;
    throw new Error(`${what}: expected a refusal naming ${reason}, but it succeeded (${sig})`);
  } catch (e) {
    if (String(e.message).includes('expected a refusal')) throw e;
    const text = `${e.message ?? ''} ${(e.logs ?? e.transactionLogs ?? []).join(' ')} ${e.error?.errorCode?.code ?? ''}`;
    if (!text.includes(reason)) throw new Error(`${what}: refused, but not with ${reason}: ${text.slice(0, 600)}`);
    ok(what, `(refused: ${reason})`);
  }
}

/** Chain time, not wall time: deadlines are checked against the Clock sysvar. */
async function chainNow() {
  const slot = await c.connection.getSlot('confirmed');
  return (await c.connection.getBlockTime(slot)) ?? Math.floor(Date.now() / 1000);
}
async function waitUntilChain(t) {
  while ((await chainNow()) <= t) await sleep(1000);
}

// ── accounts ─────────────────────────────────────────────────────────────
const run = `v${Date.now().toString(36)}`;
console.log(`verify-solana on ${slug} — run ${run}, program ${dep.programId}`);
await sweep(ROOT, c, root);

async function fundSol(to, sol) {
  const tx = new Transaction().add(SystemProgram.transfer({fromPubkey: root.publicKey, toPubkey: to, lamports: Math.round(sol * LAMPORTS_PER_SOL)}));
  await c.connection.sendTransaction(tx, [root]).then((s) => c.connection.confirmTransaction(s, 'confirmed'));
}
const newOwner = async () => {
  const kp = newKey(ROOT, c);
  await fundSol(kp.publicKey, slug === 'localnet' ? 2 : 0.05);
  return kp;
};
// The agent wallet: its own keypair, a little SOL for fees and job rent.
const newWallet = async () => {
  const kp = newKey(ROOT, c);
  await fundSol(kp.publicKey, slug === 'localnet' ? 1 : 0.03);
  return kp;
};

const client = await newOwner();
const worker = await newOwner();
const clientBot = await newWallet();
const workerBot = await newWallet();
ok('owners and their agents\' separate wallet keypairs created');

// ── token ────────────────────────────────────────────────────────────────
const ata = async (owner) => (await getOrCreateAssociatedTokenAccount(c.connection, root, mint, owner.publicKey)).address;
const clientAta = await ata(client);
const workerAta = await ata(worker);
await mintTo(c.connection, root, mint, clientAta, root, 100_000_000);
await mintTo(c.connection, root, mint, workerAta, root, 100_000_000);
ok('owners minted 100 test USDC each');

// ── registration ─────────────────────────────────────────────────────────
async function register(owner, wallet) {
  const cfg = await prog.account.config.fetch(pda.config());
  const id = cfg.nextAgentId.toNumber();
  await prog.methods
    .registerAgent(wallet.publicKey, `data:,${run}`, null, null)
    .accountsPartial({owner: owner.publicKey, config: pda.config(), agent: pda.agent(id), systemProgram: SystemProgram.programId})
    .signers([owner])
    .rpc();
  const accepted = await prog.methods
    .acceptWallet(new BN(id))
    .accountsPartial({wallet: wallet.publicKey, agent: pda.agent(id), index: pda.wallet(wallet.publicKey), previousIndex: null, systemProgram: SystemProgram.programId})
    .signers([wallet])
    .rpc();
  return {id, pda: pda.agent(id), accepted};
}
const A = await register(client, clientBot);
const B = await register(worker, workerBot);
const agentA = await prog.account.agent.fetch(A.pda);
if (!agentA.wallet.equals(clientBot.publicKey) || !agentA.owner.equals(client.publicKey)) throw new Error(`agent ${A.id}: ${JSON.stringify(agentA)}`);
await refused(
  prog.methods
    .acceptWallet(new BN(B.id))
    .accountsPartial({wallet: clientBot.publicKey, agent: B.pda, index: pda.wallet(clientBot.publicKey), previousIndex: null, systemProgram: SystemProgram.programId})
    .signers([clientBot])
    .rpc(),
  'NotPendingWallet',
  "a wallet claiming another agent it was not proposed for",
);
ok(`agents ${A.id} and ${B.id} registered; each wallet accepted by its own key`, link(A.accepted));

// ── funding ──────────────────────────────────────────────────────────────
const deposit = (method, owner, from, agentPda, amount) =>
  prog.methods[method](new BN(String(amount)))
    .accountsPartial({payer: owner.publicKey, config: pda.config(), agent: agentPda, from, vault: pda.vault(), mint, tokenProgram: TOKEN_PROGRAM_ID})
    .signers([owner])
    .rpc();
await deposit('fund', client, clientAta, A.pda, 5_000_000);
// The minimum, plus room for the bond expire_undelivered seizes below.
await deposit('stake', worker, workerAta, B.pda, BigInt(P.minStake.toString()) + 100_000n);
const a1 = await prog.account.agent.fetch(A.pda);
const b1 = await prog.account.agent.fetch(B.pda);
if (a1.purse.balance.toString() !== '5000000') throw new Error(`purse: ${a1.purse.balance}`);
if (BigInt(b1.stake.available.toString()) < BigInt(P.minStake.toString())) throw new Error('worker not hireable after staking');
ok('client funded 5 test USDC into its purse; worker staked and is hireable');

// ── escrow, happy path ───────────────────────────────────────────────────
const nextJob = async () => (await prog.account.config.fetch(pda.config())).nextJobId.toNumber();
async function hire(n, amount = 50_000, accept = 60, work = 60, wallet = clientBot, clientPda = A.pda, workerPda = B.pda) {
  const id = await nextJob();
  await prog.methods
    .createJob(new BN(amount), sha(`${run}:spec:${n}`), new BN(accept), new BN(work))
    .accountsPartial({wallet: wallet.publicKey, config: pda.config(), client: clientPda, worker: workerPda, job: pda.job(id), systemProgram: SystemProgram.programId})
    .signers([wallet])
    .rpc();
  return id;
}
const act = (method, signer, jobId, ...args) =>
  prog.methods[method](...args)
    .accountsPartial({signer: signer.publicKey, config: pda.config(), job: pda.job(jobId), client: A.pda, worker: B.pda})
    .signers(signer === root ? [] : [signer])
    .rpc();

const workerBefore = BigInt((await prog.account.agent.fetch(B.pda)).purse.balance.toString());
const j1 = await hire(1);
await act('acceptJob', workerBot, j1);
await act('submitResult', workerBot, j1, sha(`${run}:result:1`));
const approved = await act('approve', clientBot, j1, sha('judge says fine'));
const job1 = await prog.account.job.fetch(pda.job(j1));
const workerAfter = BigInt((await prog.account.agent.fetch(B.pda)).purse.balance.toString());
const fee = BigInt(job1.fee.toString());
if (!('settled' in job1.state) || !('success' in job1.outcome) || workerAfter - workerBefore !== 50_000n - fee) {
  throw new Error(`job ${j1}: ${JSON.stringify(job1)}; worker +${workerAfter - workerBefore}`);
}
ok(`escrow: created, accepted, submitted, approved — worker paid ${50_000n - fee} (fee ${fee})`, link(approved));

// ── the worker declines: the x402 cancel leg ─────────────────────────────
const balBefore = BigInt((await prog.account.agent.fetch(A.pda)).purse.balance.toString());
const jd = await hire('decline');
await refused(act('declineJob', clientBot, jd), 'NotAgentWallet', 'the client declining on the worker\'s behalf');
const declined = await act('declineJob', workerBot, jd);
const jobD = await prog.account.job.fetch(pda.job(jd));
const balAfter = BigInt((await prog.account.agent.fetch(A.pda)).purse.balance.toString());
if (!('refunded' in jobD.state) || jobD.outcome !== null || balAfter !== balBefore) {
  throw new Error(`job ${jd}: ${JSON.stringify(jobD)}; client ${balBefore} → ${balAfter}`);
}
ok('decline_job: the worker refuses an offer — the client is refunded in full at once', link(declined));
await refused(act('declineJob', workerBot, j1), 'InvalidState', 'declining a job that already settled');

// ── direct pay, caps, self-dealing, same owner ───────────────────────────
const dpId = await nextJob();
const dp = await prog.methods
  .directPay(new BN(20_000), sha(`${run}:dp`))
  .accountsPartial({wallet: clientBot.publicKey, config: pda.config(), client: A.pda, worker: B.pda, job: pda.job(dpId), systemProgram: SystemProgram.programId})
  .signers([clientBot])
  .rpc();
ok('direct_pay settles in one call', link(dp));
await refused(hire('cap', Number(P.defaultPerTaskCap.toString()) + 1), 'PerTaskCapExceeded', 'a hire above the per-task cap');
await refused(hire('self', 50_000, 60, 60, clientBot, A.pda, A.pda), 'SelfDealing', 'an agent hiring itself');
// A second agent of the WORKER's owner tries to hire the worker.
const sockBot = await newWallet();
const S = await register(worker, sockBot);
await deposit('fund', worker, workerAta, S.pda, 1_000_000);
await refused(hire('same', 50_000, 60, 60, sockBot, S.pda, B.pda), 'SameOwner', 'an agent hiring another agent of the same owner');

// ── a leaked agent key cannot withdraw ───────────────────────────────────
const withdraw = (signer, agentPda, to, amount) =>
  prog.methods
    .withdraw(new BN(amount))
    .accountsPartial({owner: signer.publicKey, config: pda.config(), agent: agentPda, to, vault: pda.vault(), mint, tokenProgram: TOKEN_PROGRAM_ID})
    .signers([signer])
    .rpc();
const thiefAta = (await getOrCreateAssociatedTokenAccount(c.connection, root, mint, clientBot.publicKey)).address;
await refused(withdraw(clientBot, A.pda, thiefAta, 1), 'NotAgentOwner', "the agent's own wallet key withdrawing its purse");
const w = await withdraw(client, A.pda, clientAta, 10_000);
ok("the owner's key withdraws", link(w));

// ── a leaked WORKER key cannot hand its stake away as bonds ──────────────
// The attacker registers its own client agent with high caps and hires the
// victim for a large job. If the stolen worker wallet could accept it, the
// attacker would let it expire undelivered and collect the seized bond.
// Bonds count toward the worker's own caps, so this is refused.
const attacker = await newOwner();
const attackerBot = await newWallet();
const attackerAta = await ata(attacker);
await mintTo(c.connection, root, mint, attackerAta, root, 10_000_000);
const X = await register(attacker, attackerBot);
await prog.methods.setCaps(new BN(5_000_000), new BN(5_000_000)).accountsPartial({owner: attacker.publicKey, agent: X.pda}).signers([attacker]).rpc();
await deposit('fund', attacker, attackerAta, X.pda, 2_000_000);
const jb = await hire('bond', 1_000_000, 60, 60, attackerBot, X.pda, B.pda);
const bondOf = BigInt((await prog.account.job.fetch(pda.job(jb))).workerBond.toString());
const workerCap = BigInt((await prog.account.agent.fetch(B.pda)).purse.perTaskCap.toString());
if (bondOf <= workerCap) throw new Error(`setup: bond ${bondOf} must exceed the worker's per-task cap ${workerCap}`);
const actX = (method, signer, jobId) =>
  prog.methods[method]()
    .accountsPartial({signer: signer.publicKey, config: pda.config(), job: pda.job(jobId), client: X.pda, worker: B.pda})
    .signers([signer])
    .rpc();
await refused(actX('acceptJob', workerBot, jb), 'PerTaskCapExceeded', `the worker's wallet key locking a ${bondOf} bond above its ${workerCap} per-task cap`);
await actX('declineJob', workerBot, jb);

// ── dispute, ruled by the arbiter ────────────────────────────────────────
const j2 = await hire(2);
await act('acceptJob', workerBot, j2);
await act('submitResult', workerBot, j2, sha(`${run}:result:2`));
await act('dispute', clientBot, j2, sha('the result is wrong'));
await refused(act('resolveDispute', clientBot, j2, false), 'NotArbiter', 'the client ruling its own dispute');
if (dep.accounts.arbiter === root.publicKey.toBase58()) {
  const ruled = await act('resolveDispute', root, j2, false);
  const job2 = await prog.account.job.fetch(pda.job(j2));
  if (!('refunded' in job2.state) || !('disputeLost' in job2.outcome)) throw new Error(`job ${j2}: ${JSON.stringify(job2)}`);
  ok('dispute ruled for the client by the arbiter: refunded, worker marked DisputeLost', link(ruled));
} else {
  ok(`dispute opened on job ${j2}; the arbiter is ${dep.accounts.arbiter} (${dep.arbiterKind}) — it rules through its own flow`);
}

// ── every exit anyone may call ───────────────────────────────────────────
const anyone = root;
const j3 = await hire(3, 50_000, 2, 60);
await waitUntilChain((await prog.account.job.fetch(pda.job(j3))).acceptDeadline.toNumber());
await act('expireUnaccepted', anyone, j3);
ok('expire_unaccepted: an offer nobody took is refunded', `job ${j3}`);

const j4 = await hire(4, 50_000, 60, 2);
await act('acceptJob', workerBot, j4);
const stakeBefore = BigInt((await prog.account.agent.fetch(B.pda)).stake.available.toString());
await waitUntilChain((await prog.account.job.fetch(pda.job(j4))).workDeadline.toNumber());
await act('expireUndelivered', anyone, j4);
const job4 = await prog.account.job.fetch(pda.job(j4));
const stakeAfter = BigInt((await prog.account.agent.fetch(B.pda)).stake.available.toString());
if (!('failed' in job4.outcome) || stakeBefore - stakeAfter !== BigInt(job4.workerBond.toString())) throw new Error(`job ${j4}: ${JSON.stringify(job4)}`);
ok("expire_undelivered: accepted and never delivered — refunded, the worker's bond seized", `job ${j4}`);

if (slug === 'localnet') {
  const j5 = await hire(5);
  await act('acceptJob', workerBot, j5);
  await act('submitResult', workerBot, j5, sha(`${run}:result:5`));
  await waitUntilChain((await prog.account.job.fetch(pda.job(j5))).reviewDeadline.toNumber());
  await act('autoApprove', anyone, j5);
  ok('auto_approve: a client that stays silent pays', `job ${j5}`);

  const j6 = await hire(6);
  await act('acceptJob', workerBot, j6);
  await act('submitResult', workerBot, j6, sha(`${run}:result:6`));
  await act('dispute', clientBot, j6, null);
  await waitUntilChain((await prog.account.job.fetch(pda.job(j6))).disputeDeadline.toNumber());
  await act('expireDispute', anyone, j6);
  const job6 = await prog.account.job.fetch(pda.job(j6));
  if (!('settled' in job6.state) || !('unresolved' in job6.outcome)) throw new Error(`job ${j6}: ${JSON.stringify(job6)}`);
  ok('expire_dispute: a dispute nobody rules on pays the worker and returns the bond', `job ${j6}`);
} else {
  console.log('  · auto_approve and expire_dispute: windows are 10 min and 1 h on devnet — checked on localnet');
}

// ── invariant I1 ─────────────────────────────────────────────────────────
const cfg = await prog.account.config.fetch(pda.config());
const t = cfg.totals;
const sum = [t.locked, t.disputeBonds, t.balances, t.stakes, t.feesOwed].reduce((s, v) => s + BigInt(v.toString()), 0n);
const held = (await getAccount(c.connection, pda.vault())).amount;
if (sum !== held) throw new Error(`I1: owes ${sum}, holds ${held}`);
ok(`I1: the vault holds exactly what the program owes — ${held} base units`, JSON.stringify(Object.fromEntries(Object.entries(t).map(([k, v]) => [k, v.toString()]))));

console.log(`\nverify-solana: ${passed} checks passed on ${slug}`);
await sweep(ROOT, c, root);
