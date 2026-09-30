# Issue #74: prune_auth_cookies only cleared the current authority

An instance window whose DSH instance restarted on a new port kept every
browser-auth cookie its earlier ports had minted. After roughly 80 restarts the
request head crossed node:http's 16 KiB `maxHeaderSize`, every request —
including the one carrying a fresh `?token=` — was answered `431` before it was
parsed, and the window could no longer refresh the cookie that would have pruned
the store. The window was then permanently `431`.

The launcher gave each instance a private WebView2 data directory, which does
bound the corpus per *instance*, but an instance that restarts reuses that same
directory: per-authority isolation never existed. What bounded the store was
`prune_auth_cookies`, and it matched the cookie name of the authority the
window was *heading for*:

    dsh-auth-<base64url(sha256(host:port))>

DSH bakes `host:port` into the name and keeps the cookie for 30 days, so after
a restart on a different port that exact name is not in the store at all: the
prune matched nothing and every earlier port's cookie survived its full 30-day
life. That is the bug.

## The fix

`prune_auth_cookies` now drops **every** `dsh-auth-*` cookie belonging to the
instance's own host, via a pure, testable predicate:

```rust
fn should_prune(name: &str, domain: Option<&str>, host: &str) -> bool {
    name.starts_with("dsh-auth-") && domain == Some(host)
}
```

The authority — and with it the port — is deliberately not compared. Within an
instance's private store, any `dsh-auth-<anything>` on that instance's own host
is one of its own earlier ports, and DSH mints a fresh signing secret per
process, so after a restart every one of them is already dead: they can only
inflate the request head.

Two constraints keep the blast radius at exactly that set:

- the store is private to the instance (`apply_window_store` ->
  `data_directory(<data_dir>/webview/<instance-id>)`), so the candidate set is
  at most this instance's own cookies; and
- `domain == Some(host)`, where `host` comes from `loopback_origin()`, which
  only ever yields `http://127.0.0.1[:port]`. WSL forwards, `localhost` and
  `https` return `None`, so `prune_auth_cookies` is not called for them at all.

The exact-name computation was removed rather than kept and masked, and the two
comments that claimed "earlier ports never accumulate" (`webview_data_dir` and
the `apply_window_store` call site) were corrected to say what is actually
true: isolation is per instance, not per authority, and pruning is what bounds
the store.

## Tests

```
windows::tests::should_prune_hits_every_authority_a_port_ever_minted
windows::tests::should_prune_leaves_foreign_cookies_alone
windows::tests::dsh_auth_cookie_name_follows_the_origin_authority  (kept)
windows::tests::loopback_origin_accepts_only_plain_loopback_http    (kept)
```

The first pins the contract: given the current authority plus two historical
ones, the prefix rule hits **3/3**, and the test also asserts that the old
exact-name rule hits only **1/3** — the regression contrast that makes the test
guard the new behaviour rather than merely describe it.

The second pins the boundary: a different authority's digest is *not* foreign
(it is the target), while a wrong prefix (`session`, `dsh-authX` — the hyphen
is part of the prefix), a case-mismatched name, a different domain, and a
missing domain are all left alone. The bare `dsh-auth-` prefix (which DSH does
not mint today) is matched on purpose and pinned with a comment, so widening the
rule stays a deliberate decision.

`dsh_auth_cookie_name_follows_the_origin_authority` still pins
`dsh-auth-k320QAAWVPdxfnOxhyOWdX-dQ03IEKrIdtzGoIirIUY`, now via a self-contained
test helper rather than production code.

## Verification

Run on Windows (cargo/rustc 1.97.0) from the repository root, each cargo command
strictly one at a time:

| Gate | Result |
|---|---|
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | EXIT=0 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets --locked -- -D warnings` | EXIT=0 |
| `cargo test --manifest-path src-tauri/Cargo.toml --workspace --locked` | EXIT=0 — 226 passed, 0 failed, 3 ignored |
| `vue-tsc --noEmit` (step 1 of `pnpm build`) | EXIT=0 |
| `vite build` (step 2 of `pnpm build`) | EXIT=0, built in 1m 7s |

`clippy -D warnings` clean is the decisive answer to the one question the fix
plan left to the compiler: removing the exact-name computation did leave
`authority`/`name` unused, and they were removed cleanly rather than hidden
behind `let _ = &name;`. Because the crate compiles only per-target, the
`base64`/`sha2` imports that the tests and the macOS/iOS store block still need
were moved to the scopes that use them instead of being blanket-`allow`ed.

The frontend gates were run as the two real steps of the `build` script rather
than through `pnpm build`, because `pnpm install` could not complete on this
machine (see below). On CI the `quality` job installs first with
`pnpm install --frozen-lockfile` and then runs `pnpm build`.

Independent verification, including a mutation check, is recorded in
[ISSUE74_VERIFICATION.md](./ISSUE74_VERIFICATION.md).

### Mutation check

Reverting the predicate to exact-name equality made
`should_prune_hits_every_authority_a_port_ever_minted` fail with
`left: 1, right: 3` — that `1` is bug #74 itself — while the naming and
loopback tests stayed green, so the mutation was surgical. The mutant had to be
made to compile first; a non-compiling mutant would have proved nothing. The
file was then restored byte-for-byte (SHA256
`966591C780B6B4EE8732564057397B6645D824420432BD6DC488746C640DD196`, git blob
`233f3aa`, unchanged against a backup taken before mutating).

## Differences from the fix plan

- **P3 (clear the instance's web data on authority change) was not taken.** It
  is more thorough but also wipes `localStorage` — user preferences and panel
  state — and P1 alone already bounds the store. Left for a separate issue, as
  the plan recommended.
- **P4 (DSH side) is out of scope here.** The cookie naming scheme is the root
  cause and only DSH can change it; this repository can only prune defensively.
  Hosts that open `dsh web` in an ordinary browser still accumulate dead
  cookies.

## Not covered

- **No manual end-to-end pass.** Acceptance criterion 5 of the fix plan —
  restarting instances repeatedly on `--port 0`, confirming the window is no
  longer `431` and that `dsh-auth-*` converges to a single cookie — needs a
  running launcher, a running instance and a look inside the WebView2 store. It
  was not performed. What is proven here is static gates plus unit tests.
- **Cross-platform behaviour is not verified locally.** Linux and macOS are
  covered only by the CI matrix. macOS takes the `clear_all_browsing_data`
  branch and never reads the data directory, so this change does not alter its
  code path.

## Environment note

Two local environment traps cost significant time and are worth recording, as
neither is a property of the change:

- A cold `cargo` run hangs indefinitely (no `rustc`, no output, forever) when
  the local package cache is incomplete — it was missing crates including
  `libc v0.2.189` and stalled on `Updating crates.io index`. `cargo fetch
  --locked` to completion (~74 MB) fixed it. It presents as a lock deadlock but
  is not one.
- `pnpm install` stalls for 16+ minutes with sustained connections to a CDN
  host and zero progress. The frontend could still be built by invoking
  `vue-tsc` and `vite` from `node_modules/.pnpm` directly, which is what the
  results above did.

## Timeline (Asia/Shanghai)

| Time | Step |
|---|---|
| 00:28 | Fork inspected: `behind 73 / ahead 0` of upstream, fast-forward possible |
| 00:30 | Fork `main` fast-forwarded to upstream `c207edf`; branch `fix/74-prune-auth-cookies` created |
| 00:31–00:36 | Fix implemented in `src-tauri/src/windows.rs` (predicate, call site, comments, tests) |
| 00:36–01:05 | cargo package-cache stall diagnosed and cleared with `cargo fetch --locked` |
| 01:05 | `fmt` green; `clippy` exposed three unused imports; `test` exposed a wrong assertion in the new boundary test |
| 01:08 | Both corrected (`Sha256`/`Digest`/`base64::Engine` scoped; the boundary assertion had encoded the *old* mental model) |
| 01:08–01:20 | All three Rust gates green; independent verification with mutation check |
| 01:30 | Frontend gates green (`vue-tsc` EXIT=0, `vite build` EXIT=0) |
| 01:3x | Report written; branch and synced `main` pushed to the fork; PR opened upstream |

Fixes #74.
