#!/usr/bin/env node
/**
 * Initialize Fidora on a cluster where the program is already deployed, and
 * write deployments/<cluster>.json.
 *
 *   node scripts/deploy.mjs --cluster localnet   # solana-test-validator with the program preloaded
 *   node --env-file=.env scripts/deploy.mjs --cluster devnet
 *
 * Creates a 6-decimal test USDC mint (its authority is the deployer, so the
 * scripts can mint test money), then `initialize` with
 * config/params.<cluster>.json `onchain`, and reads the config back: if any
 * parameter or role differs, nothing is written.
 *
 * Arbiter: ARBITER_PUBKEY (devnet: a Squads multisig vault), else the
 * deployer — stated in the deployment file, and in the README, as such.
 */
import {writeFileSync, mkdirSync} from 'node:fs';
import {dirname} from 'node:path';
import {PublicKey, SystemProgram} from '@solana/web3.js';
import {TOKEN_PROGRAM_ID, createMint} from '@solana/spl-token';
import {PROGRAM_ID, arg, cluster, deployer, deploymentFile, params, pda, program} from './lib/fidora.mjs';

const slug = arg('cluster');
const c = cluster(slug);
const root = await deployer(c);
const prog = program(c, root);

const info = await c.connection.getAccountInfo(PROGRAM_ID);
if (!info?.executable) throw new Error(`program ${PROGRAM_ID} is not deployed on ${slug}`);
if (await c.connection.getAccountInfo(pda.config())) throw new Error(`already initialized on ${slug} (config ${pda.config()}) — this script does not overwrite a deployment`);

const arbiter = process.env.ARBITER_PUBKEY ? new PublicKey(process.env.ARBITER_PUBKEY) : root.publicKey;
const p = params(slug);
console.log(`deploying Fidora on ${slug} from ${root.publicKey}`);
const mint = await createMint(c.connection, root, root.publicKey, null, 6);
console.log(`  test USDC mint ${mint}`);

const sig = await prog.methods
  .initialize(arbiter, root.publicKey, root.publicKey, p)
  .accountsPartial({
    owner: root.publicKey,
    config: pda.config(),
    mint,
    vault: pda.vault(),
    tokenProgram: TOKEN_PROGRAM_ID,
    systemProgram: SystemProgram.programId,
  })
  .rpc();
console.log(`  initialize ${sig}`);

const cfg = await prog.account.config.fetch(pda.config());
const want = JSON.stringify(Object.fromEntries(Object.entries(p).map(([k, v]) => [k, String(v)])));
const got = JSON.stringify(Object.fromEntries(Object.entries(cfg.params).map(([k, v]) => [k, String(v)])));
if (want !== got) throw new Error(`params on chain differ from params.${slug}.json:\n${got}\n${want}`);
if (!cfg.mint.equals(mint) || !cfg.arbiter.equals(arbiter) || !cfg.owner.equals(root.publicKey)) {
  throw new Error(`roles on chain: ${JSON.stringify({mint: cfg.mint, arbiter: cfg.arbiter, owner: cfg.owner})}`);
}
const slot = await c.connection.getSlot('confirmed');
const deployment = {
  cluster: slug,
  programId: PROGRAM_ID.toBase58(),
  deployedAt: new Date().toISOString(),
  startSlot: slot,
  accounts: {
    config: pda.config().toBase58(),
    vault: pda.vault().toBase58(),
    paymentMint: mint.toBase58(),
    arbiter: arbiter.toBase58(),
    owner: root.publicKey.toBase58(),
  },
  arbiterKind: arbiter.equals(root.publicKey) ? 'deployer key (not a multisig)' : 'ARBITER_PUBKEY',
};
mkdirSync(dirname(deploymentFile(slug)), {recursive: true});
writeFileSync(deploymentFile(slug), `${JSON.stringify(deployment, null, 2)}\n`);
console.log(`wrote deployments/${slug}.json`);
