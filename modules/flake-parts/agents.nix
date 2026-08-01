{
  config,
  lib,
  xiFlake,
  ...
}:
let
  cfg = config.xi;
in
{
  options.xi.agents = {
    enable = lib.mkEnableOption "xi-agent skills for AI coding agents";

    skillsPath = lib.mkOption {
      type = lib.types.path;
      default = xiFlake + "/crates/xi-agent/skills";
      readOnly = true;
      description = ''
        Path to the source-of-truth skills directory shipped with this
        flake. Consumers can reference it directly to install skills
        into project-local `.claude/skills` or `.codex/skills` trees.
      '';
    };

    targets = lib.mkOption {
      description = ''
        Which agent runtimes are considered enabled at the flake level.
        The flake-parts module does not install skills on its own; it
        only exposes the source path for downstream Home Manager or
        NixOS modules to consume.
      '';
      default = {
        claude-code.enable = true;
        codex.enable = true;
      };
      type = lib.types.submodule {
        options = {
          claude-code.enable = lib.mkEnableOption "Claude Code";
          codex.enable = lib.mkEnableOption "Codex";
        };
      };
    };

    mcp.enable = lib.mkEnableOption ''
      MCP server integration (reserved for a future release — no effect
      in v1)
    '';
  };

  config = lib.mkIf (cfg.enable && cfg.agents.enable) {
    # No side-effects at the flake-parts level. All install work happens
    # in Home Manager or NixOS. Read `config.xi.agents.skillsPath` from
    # a consumer to symlink or copy the source tree yourself.
  };
}
