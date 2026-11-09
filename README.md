# fidora-solana-programs

The Solana program for **Fidora**: an on-chain escrow, worker bonds, rolling
spend caps and settlement-written reputation for AI agents that hire each
other. Built with Anchor 1.2.2. Its x402 integration (agents paying each
other over HTTP) is in
[fidora-solana-backend](https://github.com/gopaltalaviya/fidora-solana-backend).

> **Encode Solana Hackathon 2026 — what existed before.** The protocol's
> rules (escrow states, caps, bonds, refusal names such as
> `PerTaskCapExceeded` or `SameOwner`) were designed by the author before
> the hackathon. This Anchor program implementing them on Solana was written
> during the hackathon: PDA accounts, an SPL-token vault, and `decline_job`,
> the x402 cancel leg.

## How Solana powers it

| Need | On Solana |
|---|---|
| Hold a client's money until the work is judged | An SPL-token **vault PDA** owned by the program; `create_job` moves the payment from the client agent's purse into escrow |
| Bound what an agent can spend | Per-task and rolling 24 h caps, in the agent's PDA and enforced by `create_job`, `dispute` and `accept_job` (the worker bond counts too), so a stolen agent key can spend at most its caps — as payments or as bonds |
| Keep the agent's key away from the money | Two keys per agent: the **owner** signs registration, caps and every withdrawal; the agent's **wallet** keypair signs only job instructions |
| Back delivery | The worker stakes; `accept_job` locks a bond, which `expire_undelivered` seizes |
| Reputation nobody can fake | Only settlement writes the worker and client records, so every review cost a payment |
| Exits that never trap money | `expire_unaccepted`, `expire_undelivered`, `auto_approve`, `expire_dispute`, callable by anyone |

Invariant **I1**: the vault's token balance equals the sum of what the
program owes (`Config.totals`). `verify-solana` checks it live.

## Layout

| Path | What |
|---|---|
| `programs/fidora/src/lib.rs` | The program: 36 instructions |
| `programs/fidora/src/budget.rs` | The rolling 24 h cap, ported line for line, with unit tests; a Clock step back neither panics nor reopens the window |
| `programs/fidora/src/{state,events,errors}.rs` | Accounts, events, refusals |
| `idl/fidora.json` | The IDL (built without the Anchor CLI; see below) |
| `artifacts/fidora.so` | The built program, sha256 in `artifacts/SHA256SUMS`; what the scripts deploy |
| `scripts/deploy.mjs` | Initialize on localnet or devnet; writes `deployments/<cluster>.json` |
| `scripts/verify-solana.mjs` | Every flow, live, with production-style keys: **23 checks** |

## Run it live

You need the Solana CLI (`solana-test-validator`) and Node 22+.

```sh
npm ci
solana-test-validator --reset --bind-address 127.0.0.1 --dynamic-port-range 18000-18200 \
  --bpf-program 5pND3A7Xqibq5h9XxY7YtuoJ5TomYKn79usirDBysNRg artifacts/fidora.so &
node scripts/deploy.mjs --cluster localnet        # → "wrote deployments/localnet.json"
node scripts/verify-solana.mjs --cluster localnet # → "verify-solana: 23 checks passed on localnet"
```

What `verify-solana` checks, every one against accounts on chain:

- registration, with the wallet accepting for itself
- funding and staking
- the escrow happy path, paid exactly `amount − fee`
- `decline_job` refunding in full
- `direct_pay`
- refusals: `PerTaskCapExceeded`, `SelfDealing`, `SameOwner`, `NotPendingWallet`, `NotArbiter`, and the agent's own key withdrawing (`NotAgentOwner`)
- the owner's withdraw
- an arbiter ruling
- all four exits
- I1

## Build

```sh
cargo test --manifest-path programs/fidora/Cargo.toml                          # 7 unit tests (rolling cap)
cargo build-sbf --tools-version v1.58 --manifest-path programs/fidora/Cargo.toml
```

The platform tools bundled with Agave 3.0 (cargo 1.84) cannot build current
dependencies, which need Rust 2024. `--tools-version v1.58` fetches the
current official platform tools.

The IDL comes from the `idl-build` test target, assembled by
`scripts/build-idl.mjs`. That script follows the same steps as
`anchor-lang-idl` 0.1.4:

```sh
cargo test --manifest-path programs/fidora/Cargo.toml __anchor_private_print_idl --features idl-build -- --show-output --quiet > idl.out
node scripts/build-idl.mjs idl.out > idl/fidora.json
```

## Devnet

The deployer is **your own wallet**: `FIDORA_DEPLOYER_KEYPAIR` in `.env` names
a keypair file you hold. It pays for everything, is the test-USDC mint
authority and the program's upgrade authority. No script asks you to fund an
address it made up.

`node --env-file=.env scripts/deploy.mjs --cluster devnet` needs:
- the program deployed at its id, with `solana program deploy -u devnet`
  (the deployer pays the program's rent and can reclaim it later with
  `solana program close`)
- `FIDORA_DEPLOYER_KEYPAIR` in `.env`

Test accounts on devnet: `verify-solana` makes fresh owner and agent-wallet
keys each run. Each key is written to `.keys/devnet/` (gitignored) before
any SOL is sent to it. At the end of the run, everything they hold goes back
to the deployer: the SOL, plus the rent of their token accounts. The same
happens at the start of the next run, in case a run stopped half-way. To
sweep by hand: `npm run sweep:devnet`.

**Deployed on devnet** (2026-11-08), `deployments/devnet.json`:

| | Address |
|---|---|
| Program | [`5pND3A7Xqibq5h9XxY7YtuoJ5TomYKn79usirDBysNRg`](https://explorer.solana.com/address/5pND3A7Xqibq5h9XxY7YtuoJ5TomYKn79usirDBysNRg?cluster=devnet) |
| Upgrade authority, owner, arbiter (the deployer) | `2m9UHYVSk6V7ErSycjhQdev2RqjGymYsgx2aozhC19eG` |
| Config | `4XzNRdtZo4RWWGNFJx2MoDYQ8qfHyrNk3nCkgCJ6E7dD` |
| Vault | `2jc5eNfZxhWQ6Da5kR7JpF86kDYyCum9JQrF6cGNC8yu` |
| Test USDC mint | `34rRkNq7UHkyKXdXSML899EcHKN11PFUR1P9ft94QTtA` |

`verify-solana --cluster devnet`: 21 checks passed on devnet, each with its
explorer link. `auto_approve` and `expire_dispute` wait out 10-minute and
1-hour windows there, so they are checked on localnet (23/23).

The public devnet RPC answers bursts with 429, and refuses a batch of 10 or
more `getTransaction` calls outright. The scripts wait those out
(`patientFetch` in `scripts/lib/fidora.mjs`); a private devnet RPC in
`RPC_URL_DEVNET` avoids the waits. The scripts refuse mainnet.

## Licence

MIT. See `LICENSE`.
