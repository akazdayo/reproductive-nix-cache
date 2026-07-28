{
  description = "A Nix-flake-based Rust development environment";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1"; # unstable Nixpkgs
    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rustfs = {
      url = "github:rustfs/rustfs-flake";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, ... }@inputs:

    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forEachSupportedSystem =
        f:
        inputs.nixpkgs.lib.genAttrs supportedSystems (
          system:
          f {
            inherit system;
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [
                inputs.self.overlays.default
              ];
            };
          }
        );
    in
    {
      overlays.default = final: prev: {
        rustToolchain =
          with inputs.fenix.packages.${prev.stdenv.hostPlatform.system};
          combine (
            with stable;
            [
              clippy
              rustc
              cargo
              rustfmt
              rust-src
            ]
          );
      };

      checks = forEachSupportedSystem (
        { system, ... }:
        {
          pre-commit-check = inputs.git-hooks.lib.${system}.run {
            src = ./.;
            hooks = {
              treefmt = {
                enable = true;
                package = self.formatter.${system};
              };
            };
          };
        }
      );

      devShells = forEachSupportedSystem (
        { pkgs, system }:
        {
          default = pkgs.mkShell {
            packages =
              (with pkgs; [
                rustToolchain
                openssl
                pkg-config
                cargo-deny
                cargo-edit
                cargo-watch
                rust-analyzer
                nix-output-monitor
                minio-client
                inputs.rustfs.packages.${system}.default
                self.formatter.${system}
              ])
              ++ self.checks.${system}.pre-commit-check.enabledPackages;

            shellHook = ''
              ${self.checks.${system}.pre-commit-check.shellHook}
              mkdir -p "$RUSTFS_VOLUMES"
            '';

            env = {
              # Required by rust-analyzer
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";

              # Loopback-only RustFS configuration for local development.
              RUSTFS_ACCESS_KEY = "rustfsadmin";
              RUSTFS_SECRET_KEY = "rustfs-development";
              RUSTFS_VOLUMES = ".rustfs/data";
              RUSTFS_ADDRESS = "127.0.0.1:9000";
              RUSTFS_CONSOLE_ENABLE = "true";
              RUSTFS_CONSOLE_ADDRESS = "127.0.0.1:9001";

              # Nix uses the AWS credential provider chain for S3 stores.
              AWS_ACCESS_KEY_ID = "rustfsadmin";
              AWS_SECRET_ACCESS_KEY = "rustfs-development";
              AWS_DEFAULT_REGION = "us-east-1";
              NIX_CACHE_S3_URL = "s3://nix-cache?scheme=http&endpoint=127.0.0.1:9000&region=us-east-1";
            };
          };
        }
      );

      formatter = forEachSupportedSystem (
        { pkgs, ... }:
        inputs.treefmt-nix.lib.mkWrapper pkgs {
          projectRootFile = "flake.nix";
          programs = {
            nixfmt.enable = true;
            rustfmt.enable = true;
          };
        }
      );
    };
}
