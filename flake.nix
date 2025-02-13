{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    import-cargo.url = "github:edolstra/import-cargo";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    flake-utils,
    import-cargo,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {inherit system overlays;};
        inherit (import-cargo.builders) importCargo;

        rust = pkgs.rust-bin.nightly.latest.default;

        devInputs =
          (with pkgs; [
            alejandra
            black
            poetry
            sea-orm-cli
            postgresql
          ])
          ++ [
            pgstart
            pgstop
          ];
        buildInputs = [pkgs.openssl];
        nativeBuildInputs = [rust pkgs.pkg-config];

        package = pkgs.stdenv.mkDerivation {
          name = "anoquebot";
          src = self;

          inherit buildInputs;

          nativeBuildInputs =
            nativeBuildInputs
            ++ [
              (importCargo {
                lockFile = ./Cargo.lock;
                inherit pkgs;
              })
              .cargoHome
            ];

          buildPhase = ''
            cargo build --release --offline
          '';

          installPhase = ''
            install -Dm775 ./target/release/anoquebot $out/bin/anoquebot
          '';
        };

        pgstart = pkgs.writeShellScriptBin "pgstart" ''
          if [ ! -d $PGHOST ]; then
            mkdir -p $PGHOST
          fi
          if [ ! -d $PGDATA ]; then
            echo 'Initializing postgresql database...'
            LC_ALL=C.utf8 initdb $PGDATA --auth=trust >/dev/null
          fi
          OLD_PGDATABASE=$PGDATABASE
          export PGDATABASE=postgres
          pg_ctl start -l $LOG_PATH -o "-c listen_addresses= -c unix_socket_directories=$PGHOST"
          psql -tAc "SELECT 1 FROM pg_database WHERE datname = 'anoquebot'" | grep -q 1 || psql -tAc 'CREATE DATABASE "anoquebot"'
          export PGDATABASE=$OLD_PGDATABASE
        '';

        pgstop = pkgs.writeShellScriptBin "pgstop" ''
          pg_ctl -D $PGDATA stop | true
        '';
      in {
        packages.default = package;
        devShells = {
          default = pkgs.mkShell {
            buildInputs = devInputs ++ buildInputs ++ nativeBuildInputs;

            LD_LIBRARY_PATH = "${
              pkgs.lib.makeLibraryPath
              buildInputs
            }";

            shellHook = ''
              export PGDATA=$PWD/postgres/data
              export PGHOST=$PWD/postgres
              export LOG_PATH=$PWD/postgres/LOG
              export PGDATABASE=anoquebot
              export DATABASE_URL=postgresql:///anoquebot?host=$PWD/postgres;
            '';
          };
        };
      }
    );
}
