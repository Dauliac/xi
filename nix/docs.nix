{
  perSystem =
    {
      pkgs,
      lib,
      ...
    }:
    let
      repoRoot = ../.;

      # Source tree: README + Diataxis sections + assets.
      docsSrc = lib.fileset.toSource {
        root = repoRoot;
        fileset = lib.fileset.unions [
          (lib.fileset.maybeMissing (repoRoot + "/README.md"))
          (repoRoot + "/docs")
        ];
      };

      # mdBook input: assemble `src/` (markdown + SUMMARY) + `book.toml`.
      mdbookInput = pkgs.runCommand "xi-docs-mdbook-input" { } ''
        mkdir -p $out/src
        cp -r ${docsSrc}/docs/. $out/src/
        if [ -f ${docsSrc}/README.md ]; then
          cp ${docsSrc}/README.md $out/src/README.md
        fi

        # Minimal SUMMARY.md: enumerates every markdown file under src/
        # Diataxis buckets so the TOC stays coherent even without curated
        # ordering. Replace with a hand-written SUMMARY.md in docs/ if a
        # specific order is wanted.
        {
          echo "# Summary"
          echo
          if [ -f $out/src/README.md ]; then
            echo "- [Overview](./README.md)"
          fi
          for section in tutorials guides reference explanation; do
            [ -d $out/src/$section ] || continue
            # Capitalize the section title (POSIX way).
            title=$(echo "$section" | awk '{ print toupper(substr($0,1,1)) substr($0,2) }')
            echo
            echo "# $title"
            echo
            find $out/src/$section -name '*.md' -type f | sort | while read -r f; do
              rel=''${f#$out/src/}
              # Derive the heading from the first H1 if present, else filename.
              heading=$(grep -m1 '^# ' "$f" 2>/dev/null | sed 's/^# //' || true)
              [ -z "$heading" ] && heading=$(basename "$f" .md)
              echo "- [$heading](./$rel)"
            done
          done
        } > $out/src/SUMMARY.md

        cat > $out/book.toml <<'TOML'
        [book]
        title = "xi"
        description = "Documentation for xi  -  a nix CLI helper"
        authors = ["Dauliac"]
        language = "en"
        src = "src"

        [output.html]
        default-theme = "ayu"
        preferred-dark-theme = "ayu"
        git-repository-url = "https://github.com/Dauliac/xi"
        edit-url-template = "https://github.com/Dauliac/xi/edit/main/docs/{path}"
        TOML
        sed -i 's/^        //' $out/book.toml
      '';
    in
    {
      legacyPackages.docs = pkgs.runCommandLocal "xi-docs" {
        nativeBuildInputs = [ pkgs.mdbook ];
      } ''
        cp -r ${mdbookInput}/. build/
        chmod -R u+w build/
        cd build
        mdbook build
        mv book $out
      '';
    };
}
