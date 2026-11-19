{
  description = "argus: an agent development environment built around review, verification, and raw agent logs";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Rust toolchain for the std-only workspace (edition 2024).
      rustTools = pkgs: [
        pkgs.rustc
        pkgs.cargo
        pkgs.clippy
        pkgs.rustfmt
        pkgs.rust-analyzer
        pkgs.cargo-nextest
      ];

      # Formal verification: TLC for the TLA+ specs, Lean 4 (with lake) for the proofs.
      formalTools = pkgs: [
        pkgs.tlaplus18
        pkgs.jdk21_headless
        pkgs.lean4
      ];

      # Frontends: the Tauri desktop app, the Expo mobile app, and the shared TS client.
      frontendTools = pkgs: [
        pkgs.nodejs_22
        pkgs.pnpm
        pkgs.cargo-tauri
        pkgs.eas-cli
      ];

      # System libraries Tauri v2 links against on Linux (WebKitGTK 4.1, libsoup 3).
      tauriLibs = pkgs: [
        pkgs.webkitgtk_4_1
        pkgs.gtk3
        pkgs.libsoup_3
        pkgs.glib
        pkgs.cairo
        pkgs.pango
        pkgs.gdk-pixbuf
        pkgs.atk
        pkgs.openssl
        pkgs.librsvg
      ];

      # The host runs agents under a PTY with util-linux `script -qfec` (BSD `script` on
      # macOS takes other flags).
      hostTools = pkgs: pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.util-linux ];

      lintTools = pkgs: [
        pkgs.git
        pkgs.ripgrep
        pkgs.nixfmt
        pkgs.actionlint
        pkgs.typos
      ];
    in
    {
      formatter = forAllSystems (pkgs: pkgs.nixfmt);

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages =
            rustTools pkgs
            ++ formalTools pkgs
            ++ frontendTools pkgs
            ++ hostTools pkgs
            ++ lintTools pkgs
            ++ [ pkgs.pkg-config ];
          buildInputs = tauriLibs pkgs;
        };
      });
    };
}
