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
  # Map every enabled agent target to its skill install root under the
  # user's home. Structure is stable across Claude Code and Codex per
  # the Agent Skills convention (agentskills.io).
  targetPrefix =
    target:
    {
      claude-code = ".claude/skills";
      codex = ".codex/skills";
    }
    .${target};
  enabledTargets = builtins.attrNames (
    lib.filterAttrs (_: t: t.enable) cfg.agents.targets
  );
  filesFor =
    target:
    lib.listToAttrs (
      map (name: {
        name = "${targetPrefix target}/${name}/SKILL.md";
        value = {
          source = skillSrc name;
        };
      }) skillNames
    );
in
{
  options.programs.xi.agents = {
    enable =
      lib.mkEnableOption "xi-agent skills for AI coding agents";

    targets = lib.mkOption {
      description = ''
        Which agent runtimes should receive the xi skills.
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
    home.file = lib.mkMerge (map filesFor enabledTargets);
  };
}
