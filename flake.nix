{
  description = "cm-rs: Content Manager for Assetto Corsa, in Rust (GPUI)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in
    {
      packages = forAll (
        pkgs:
        let
          lib = pkgs.lib;
          cefPlatform = if pkgs.stdenv.hostPlatform.isAarch64 then "linuxarm64" else "linux64";
          cefArchive = {
            type = "minimal";
            name = "cef_binary_151.3.16+gbe1e15d+chromium-151.0.7922.109_${cefPlatform}_minimal.tar.bz2";
            sha1 =
              if pkgs.stdenv.hostPlatform.isAarch64 then
                "f1a82ce3bde5ea4338da5a41e0bcae22265f36f9"
              else
                "13042a73aeaf0e853a719d3ddad0751514665bb9";
          };
          # Metadata comes from the official CEF index. Fail on ABI drift rather
          # than letting cef-rs download an unpatched runtime during a build.
          cef-runtime =
            assert pkgs.cef-binary.version == "151.3.16";
            pkgs.runCommand "cm-rs-cef-runtime-151.3.16" { } ''
              mkdir -p $out
              for entry in ${pkgs.cef-binary}/*; do
                ln -s "$entry" "$out/$(basename "$entry")"
              done
              for entry in ${pkgs.cef-binary}/Release/* ${pkgs.cef-binary}/Resources/*; do
                name=$(basename "$entry")
                # Never put CEF's SwiftShader Vulkan loader on GPUI's library path.
                if [ "$name" != libvulkan.so.1 ]; then
                  ln -s "$entry" "$out/$name"
                fi
              done
              printf '%s' '${builtins.toJSON cefArchive}' > $out/archive.json
            '';

          # Libraries GPUI dlopens at runtime (not linked), so they must be on
          # LD_LIBRARY_PATH for the installed binary.
          runtimeLibs = with pkgs; [
            vulkan-loader
            wayland
            libxkbcommon
            libx11
            libxcb
            libxkbfile
            fontconfig
            freetype
          ];

          desktopItem = pkgs.makeDesktopItem {
                name = "cm-rs";
                desktopName = "Content Manager (Rust)";
                genericName = "Assetto Corsa Launcher";
                comment = "Native launcher for Assetto Corsa: Quick Drive, cars, tracks, mod install";
                exec = "cm-rs";
                icon = "cm-rs";
                terminal = false;
                categories = [ "Game" ];
                keywords = [ "Assetto Corsa" "AC" "Racing" "Content Manager" "Proton" ];
                startupWMClass = "cm-rs";
              };

          cm-rs-src = pkgs.rustPlatform.buildRustPackage {
            pname = "cm-rs";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./crates
                ./packaging
              ];
            };

            cargoLock.lockFile = ./Cargo.lock;
            CEF_PATH = "${cef-runtime}";
            cargoBuildFlags = [
              "-p"
              "cm-app"
            ];
            doCheck = false;

            dontUseCmakeConfigure = true;
            dontUseNinjaBuild = true;
            dontUseNinjaInstall = true;
            dontUseNinjaCheck = true;

            nativeBuildInputs = with pkgs; [
              pkg-config
              cmake
              ninja
              makeWrapper
              copyDesktopItems
              installShellFiles
            ];

            buildInputs = with pkgs; [
              fontconfig
              freetype
              libxkbcommon
              wayland
              libxcb
              libx11
              vulkan-loader
              openssl
              zstd
            ];

            desktopItems = [ desktopItem ];

            postInstall = ''
              for s in 48 64 128 256 512; do
                install -Dm644 packaging/icons/cm-rs-$s.png \
                  $out/share/icons/hicolor/''${s}x''${s}/apps/cm-rs.png
              done
            '';

            postFixup = ''
              wrapProgram $out/bin/cm-rs \
                --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}:${cef-runtime} \
                --set CM_RS_CEF_PATH ${cef-runtime} \
                --prefix PATH : ${lib.makeBinPath [ pkgs.xdg-utils ]}
            '';

            meta = {
              description = "Content Manager for Assetto Corsa, rewritten in Rust with GPUI";
              license = lib.licenses.mspl;
              mainProgram = "cm-rs";
              platforms = systems;
            };
          };
          release = builtins.fromJSON (builtins.readFile ./packaging/release.json);

          # Minimal install of the published binary release: the executable, the
          # CEF runtime files it loads, a desktop entry and icons. Nothing else.
          cm-rs = pkgs.stdenv.mkDerivation {
            pname = "cm-rs";
            inherit (release) version;

            src = pkgs.fetchurl {
              url = "https://github.com/Hy4ri/cm-rs/releases/download/v${release.version}/cm-rs-v${release.version}-linux-x86_64.tar.gz";
              hash = release.hash;
            };

            nativeBuildInputs = with pkgs; [ autoPatchelfHook makeWrapper copyDesktopItems ];
            buildInputs = with pkgs; [
              stdenv.cc.cc.lib
              libxkbcommon libxcb libx11 libxcomposite libxdamage libxext
              libxfixes libxrandr libgbm libglvnd
              glib nspr nss atk at-spi2-atk at-spi2-core dbus expat cups cairo pango
              alsa-lib systemd
            ];

            installPhase = ''
              runHook preInstall
              mkdir -p $out/libexec/cm-rs $out/bin
              cp -r . $out/libexec/cm-rs
              for s in 48 64 128 256 512; do
                install -Dm644 ${./packaging/icons}/cm-rs-$s.png \
                  $out/share/icons/hicolor/''${s}x''${s}/apps/cm-rs.png
              done
              runHook postInstall
            '';

            desktopItems = [ desktopItem ];

            # GPUI dlopens these at runtime, so autoPatchelf cannot see them.
            postFixup = ''
              makeWrapper $out/libexec/cm-rs/cm-rs $out/bin/cm-rs \
                --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}:$out/libexec/cm-rs \
                --set CM_RS_CEF_PATH $out/libexec/cm-rs \
                --prefix PATH : ${lib.makeBinPath [ pkgs.xdg-utils ]}
            '';

            meta = {
              description = "Content Manager for Assetto Corsa, rewritten in Rust with GPUI";
              license = lib.licenses.mspl;
              mainProgram = "cm-rs";
              platforms = [ "x86_64-linux" ];
              sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
            };
          };
        in
        {
          inherit cm-rs cm-rs-src cef-runtime;
          default = cm-rs;
        }
      );

      apps = forAll (pkgs: rec {
        cm-rs = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.cm-rs}/bin/cm-rs";
        };
        default = cm-rs;
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.cm-rs-src ];
          packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer ];
          # Same dlopen'd libraries as the wrapped binary, so `cargo run` works.
          LD_LIBRARY_PATH =
            (pkgs.lib.makeLibraryPath (
              with pkgs;
              [
                vulkan-loader
                wayland
                libxkbcommon
                libx11
                libxcb
                fontconfig
                freetype
              ]
            ))
            + ":${self.packages.${pkgs.stdenv.hostPlatform.system}.cef-runtime}";
          CEF_PATH = "${self.packages.${pkgs.stdenv.hostPlatform.system}.cef-runtime}";
          CM_RS_CEF_PATH = "${self.packages.${pkgs.stdenv.hostPlatform.system}.cef-runtime}";
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });

      overlays.default = final: _prev: {
        cm-rs = self.packages.${final.stdenv.hostPlatform.system}.cm-rs;
      };

      formatter = forAll (pkgs: pkgs.nixpkgs-fmt);
    };
}
