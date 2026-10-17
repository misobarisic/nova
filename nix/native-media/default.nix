# Native builds live in nova-vendor; app shells consume immutable release bundles.
{
  system ? builtins.currentSystem,
  pkgs ?
    let
      lock = builtins.fromJSON (builtins.readFile ../../flake.lock);
      source = builtins.fetchTree lock.nodes.nixpkgs.locked;
    in
    import source { inherit system; },
}:
let
  inherit (pkgs) lib;
  release = builtins.fromJSON (builtins.readFile ./release.json);
  bundles = lib.mapAttrs (
    target: pin:
    pkgs.fetchzip {
      name = "nova-native-media-${target}-${release.release}";
      url = "https://github.com/${release.repository}/releases/download/${release.release}/${pin.file}";
      inherit (pin) hash;
      stripRoot = false;
    }
  ) release.bundles;
in
lib.foldlAttrs (
  acc: target: bundle:
  acc
  // {
    "bundle-${target}" = bundle;
    "notices-${target}" = pkgs.runCommand "nova-native-notices-${target}" { } ''
      printf 'NOVA NATIVE MEDIA DEPENDENCIES\n\n' > "$out"
      cat ${bundle}/share/nova-native/sources.json >> "$out"
      printf '\n\n' >> "$out"
      while IFS= read -r license; do
        printf '\n===== %s =====\n\n' "''${license#${bundle}/share/licenses/}" >> "$out"
        cat "$license" >> "$out"
      done < <(find ${bundle}/share/licenses -type f | sort)
    '';
  }
) { } bundles
