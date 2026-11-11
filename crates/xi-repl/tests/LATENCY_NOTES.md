# Warm-eval round-trip latency (task 4.5)

`openspec/changes/xi-repl/tasks.md` task 4.5 asks Phase 0 to measure warm-eval
round-trip latency for a representative expression against a nixpkgs-sized
flake, because that number informs proposal Open Question 3 (stay on the
daemon + `nix eval` path vs. add an in-process `tvix-eval` hot path later for
completion specifically).

**No real number is recorded here yet.** This sandbox has no nixpkgs-sized flake
checked out and no long-lived shell to warm a daemon against, so fabricating a
figure would be worse than leaving the gap explicit. What follows is the
methodology the next person (or agent) with a real nixpkgs checkout should run.

## Methodology

1. Pick a representative expression. The proposal's own example is `pkgs.ne` →
   `pkgs.nebula` - i.e. attribute access one level into a
   `legacyPackages.<system>`-shaped flake. For a round-trip number, drop the
   dot-completion part and measure a concrete leaf, e.g.:

   ```text
   pkgs.hello
   ```

   against a flake whose `pkgs` is nixpkgs' `legacyPackages.<system>`.

2. Start a daemon scoped to that flake and let it complete at least one full
   eval cycle first, so `ServerState`'s `eval_input_hash` is fresh (the "warm"
   half of "warm-eval"):

   ```sh
   xi develop daemon start --flake /path/to/nixpkgs-flake &
   # wait for `xi develop status` to report `ready`
   ```

3. Time a single `ExprEval` round-trip directly against the socket, outside the
   `reedline` loop, so keystroke/terminal latency doesn't contaminate the
   number. The cleanest way to do that with what exists today is a tiny
   throwaway caller of `xi_develop::daemon::client_v3::expr_eval` (the same
   function `xi-repl`'s `session::eval` calls), timed with `std::time::Instant`:

   ```rust
   let socket = xi_develop::dirs::daemon_socket_path(&fid);
   let start = std::time::Instant::now();
   let _ = xi_develop::daemon::client_v3::expr_eval(&socket, "pkgs.hello", false);
   println!("{:?}", start.elapsed());
   ```

   Run it a handful of times (first call pays connection setup; later calls show
   steady state) and report both.

4. Compare against `xi flake show`'s own cold-start cost on the same flake
   (`time xi flake show /path/to/nixpkgs-flake`) - SC-REPL-001 is "`xi repl`
   reaches an interactive prompt in no worse time than `xi flake show` takes",
   which is a _different_ measurement (process startup + first render) from this
   task's warm-eval round-trip, but the two numbers should be recorded side by
   side since they answer related questions about where the daemon path's cost
   actually sits.

## What would make this task done

Replace this section with:

- the measured round-trip (p50 and a worse-case sample) for the expression
  above, against a real nixpkgs-sized flake,
- the nixpkgs revision / flake used, so the number is reproducible,
- a one-line verdict: is the daemon + `nix eval` round-trip fast enough that
  Open Question 3's `tvix-eval` hot path is a "later, if ever" rather than a
  "needed before Phase 1 ships" - Phase 1's completion-on-Tab UX is the first
  place this number actually matters.
