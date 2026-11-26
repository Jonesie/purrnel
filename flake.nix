{
  description = "Purrnel: an operating system that is just an AI agent, fronted by a cat";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      p = import ./nix/packages.nix { inherit pkgs; src = ./.; };
    in
    {
      packages.${system} = p // {
        default = p.purrnel;
      };
    };
}
