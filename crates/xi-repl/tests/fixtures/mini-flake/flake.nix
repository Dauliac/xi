{
  description = "Tiny fixture flake for xi-repl's end-to-end happy-path BDD test";

  # `bdd_repl_happy_path.rs` never actually runs `nix eval` against this
  # file  -  its `MiniDaemon` answers `ExprEval` queries with canned replies,
  # the same way `xi-develop`'s own `bdd_client_v3.rs` fixture daemon
  # stands in for a real `xi-develop` daemon elsewhere in this workspace.
  # This file still exists on disk (and still describes a real, if cheap,
  # derivation shape) so "opening against the fixture flake" canonicalizes
  # an actual flake root, and so the attribute names referenced by the test
  # (`pkgs.hello`, `pkgs.world`) read as a plausible flake rather than an
  # arbitrary string.
  outputs = { self }: {
    packages.x86_64-linux = {
      hello = derivation {
        name = "hello-2.12";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo hi > $out" ];
        meta.description = "A friendly test program";
      };
      world = derivation {
        name = "world-1.0";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo hi > $out" ];
      };
    };
  };
}
