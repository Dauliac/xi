{
  config,
  lib,
  xiFlake,
  ...
}:
let
  cfg = config.programs.xi;
  skillNames = [
    "xi-agent-context"
    "xi-flake-outputs"
    "xi-devshell-state"
    "xi-stage-and-manifest"
    "xi-validate-changes"
  ];
  skillSrc =
    name: xiFlake + "/crates/xi-agent/skills/${name}/SKILL.md";
  # System-wide install lands under /etc so every user picks it up when
  # HM is unavailable. Individual users can still opt into HM-level
  # install on top of this.
  targetEtcRoot =
    target:
    {
      claude-code = "xi/agents/claude-code/skills";
      codex = "xi/agents/codex/skills";
    }
    .${target};
  enabledTargets = builtins.attrNames (
    lib.filterAttrs (_: t: t.enable) cfg.agents.targets
  );
  etcFor =
    target:
    lib.listToAttrs (
      map (name: {
        name = "${targetEtcRoot target}/${name}/SKILL.md";
        value = {
          source = skillSrc name;
        };
      }) skillNames
    );
in
{
  options.programs.xi.agents = {
    enable = lib.mkEnableOption "system-wide xi-agent skills";

    targets = lib.mkOption {
      description = ''
        Which agent runtimes should receive the xi skills at system
        level (under /etc/xi/agents).
      '';
      default = {
        claude-code.enable = true;
        codex.enable = true;
      };
      type = lib.types.submodule {
        options = {
          claude-code.enable = lib.mkEnableOption "Claude Code skills";
          codex.enable = lib.mkEnableOption "Codex skills";
        };
      };
    };

    mcp.enable = lib.mkEnableOption ''
      MCP server integration (reserved for a future release — no effect
      in v1)
    '';
  };

  config = lib.mkIf (cfg.enable && cfg.agents.enable) {
    environment.etc = lib.mkMerge (map etcFor enabledTargets);
  };
}
