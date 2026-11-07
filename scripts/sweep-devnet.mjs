#!/usr/bin/env node
/**
 * Return everything the saved devnet test keys hold to the deployer (your
 * wallet): token accounts closed for their rent, then all SOL.
 *
 *   node --env-file=.env scripts/sweep-devnet.mjs
 *
 * verify-solana already sweeps at its start and end; this is for a run that
 * stopped half-way. A key whose sweep fails keeps its file in .keys/devnet/.
 */
import {ROOT, cluster, deployer} from './lib/fidora.mjs';
import {keysDir, sweep} from './lib/throwaway.mjs';

const c = cluster('devnet');
const to = await deployer(c);
const back = await sweep(ROOT, c, to);
console.log(`sweep-devnet: ${back / 1e9} SOL returned to ${to.publicKey.toBase58()} (keys dir ${keysDir('.', c)})`);
