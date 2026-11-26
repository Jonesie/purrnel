{
  description = "Purrnel: an operating system that is just an AI agent, fronted by a cat";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      # The release workflow sets PURRNEL_VERSION to the git tag and builds with --impure so it can be
      # read here; every other build says "dev" (which also keeps its Rust build cacheable).
      tag = builtins.getEnv "PURRNEL_VERSION";
      p = import ./nix/packages.nix {
        inherit pkgs;
        src = ./.;
        releaseVersion = if tag == "" then "dev" else tag;
      };
    in
    {
      packages.${system} = p // {
        default = p.purrnel;
      };
    };
}
