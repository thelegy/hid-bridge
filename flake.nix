{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
    rust-overlay,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };

      INPUT_EVENT_CODES_PATH = "${pkgs.linuxHeaders}/include/linux/input-event-codes.h";

      selectToolchain = p:
        p.rust-bin.stable.latest.default.override {
          targets = ["thumbv6m-none-eabi"];
        };

      craneLib = (crane.mkLib pkgs).overrideToolchain selectToolchain;

      sourceFilter = path: type:
        (pkgs.lib.strings.hasSuffix "/memory.x" path) || (craneLib.filterCargoSources path type);

      commonArgs = {
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = sourceFilter;
          name = "hid-bridge-source";
        };
        strictDeps = true;

        cargoExtraArgs = "--target thumbv6m-none-eabi";

        # Tests currently need to be run via `cargo wasi` which
        # isn't packaged in nixpkgs yet...
        doCheck = false;

        buildInputs =
          [
            # Add additional build inputs here
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            # Additional darwin specific inputs can be set here
            #pkgs.libiconv
          ];
      };

      cargoArtifacts = craneLib.buildDepsOnly (commonArgs
        // {
          pname = "hid-bridge-deps";

          inherit INPUT_EVENT_CODES_PATH;
        });

      hid-bridge-rp = craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;

          nativeBuildInputs = [
            pkgs.elf2uf2-rs
          ];

          postInstall = ''
            mv $out/bin/hid-bridge $out/hid-bridge.elf
            chmod -x $out/hid-bridge.elf
            rmdir $out/bin
            ${pkgs.binutils}/bin/readelf -h $out/hid-bridge.elf
            elf2uf2-rs $out/hid-bridge.elf $out/hid-bridge.uf2
          '';
        });

      watch = pkgs.writeScriptBin "watch" ''
        cargo watch --clear --delay .1 -x 'clippy'
      '';
    in {
      checks = {
        inherit hid-bridge-rp;
      };

      packages.default = hid-bridge-rp;

      devShells.default = craneLib.devShell {
        checks = self.checks.${system};

        inherit INPUT_EVENT_CODES_PATH;

        packages = [
          pkgs.probe-rs
          ((selectToolchain pkgs).override {
            extensions = ["rust-analyzer" "rust-src"];
          })
          pkgs.cargo-watch
          watch
        ];
      };
    });
}
