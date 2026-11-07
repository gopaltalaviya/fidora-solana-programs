#!/usr/bin/env node
/**
 * Assemble the Anchor IDL from the `idl-build` test output — the same steps
 * `anchor-lang-idl` 0.1.4 `build_idl` takes (address, constants, events and
 * their types, errors, program; then module paths shortened when they do not
 * clash, then everything sorted by name). Lets the IDL be rebuilt without
 * the Anchor CLI:
 *
 *   cargo test --manifest-path programs/fidora/Cargo.toml __anchor_private_print_idl \
 *     --features idl-build -- --show-output --quiet > idl.out
 *   node scripts/build-idl.mjs idl.out > idl/fidora.json
 */
import {readFileSync} from 'node:fs';

const lines = readFileSync(process.argv[2], 'utf8').split(/\r?\n/);
let address = '';
const constants = [];
const events = [];
let errors;
const types = new Map();
let idl;
let state = 'pass';
let buf = [];

for (const line of lines) {
  if (state === 'pass') {
    if (line === '--- IDL begin address ---') state = 'address';
    else if (line === '--- IDL begin const ---') [state, buf] = ['const', []];
    else if (line === '--- IDL begin event ---') [state, buf] = ['event', []];
    else if (line === '--- IDL begin errors ---') {
      if (errors) throw new Error('Multiple error definitions are not allowed.');
      [state, buf] = ['errors', []];
    } else if (line === '--- IDL begin program ---') [state, buf] = ['program', []];
    continue;
  }
  if (state === 'address') {
    address = line.replace(/[^A-Za-z0-9]/g, '');
    state = 'pass';
    continue;
  }
  const end = `--- IDL end ${state} ---`;
  if (line !== end) {
    buf.push(line);
    continue;
  }
  const json = JSON.parse(buf.join('\n'));
  if (state === 'const') constants.push(json);
  if (state === 'event') {
    events.push(json.event);
    for (const t of json.types) types.set(t.name, t);
  }
  if (state === 'errors') errors = json;
  if (state === 'program') idl = json;
  state = 'pass';
}
if (!idl) throw new Error("IDL doesn't exist");

idl.address = address;
idl.constants = constants;
idl.events = events;
idl.errors = errors ?? [];
for (const t of idl.types ?? []) types.set(t.name, t);
idl.types = [...types.values()];

// convert_module_paths: "a::b::Name" → "Name" unless another path ends in Name.
let text = JSON.stringify(idl);
for (const m of [...text.matchAll(/"((?:\w+::)+)(\w+)"/g)]) {
  const replaced = text.replaceAll(m[0], `"${m[2]}"`);
  if (!new RegExp(`"(\\w+::)+${m[2]}"`).test(replaced)) text = replaced;
}
idl = JSON.parse(text);

const byName = (a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0);
for (const k of ['accounts', 'constants', 'events', 'instructions', 'types']) idl[k]?.sort(byName);

process.stdout.write(`${JSON.stringify(idl, null, 2)}\n`);
