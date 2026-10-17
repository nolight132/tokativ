self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.tokativ;
  toml = pkgs.formats.toml { };
  models = import ./models.nix;
  inherit (pkgs.stdenv.hostPlatform) isLinux isDarwin;

  # The settings file and the prefetched models, by their paths under the app's config and data
  # folders.
  configFiles = lib.optionalAttrs (cfg.settings != { }) {
    "tokativ/config.toml" = {
      source = toml.generate "tokativ-config.toml" cfg.settings;
      force = true;
    };
  };
  modelFiles = lib.mergeAttrsList (
    map (
      name:
      let
        model = models.${name};
        folder = "tokativ/models/${lib.replaceStrings [ "/" ] [ "--" ] model.repo}";
        stem = lib.removeSuffix ".gguf" model.file;
      in
      {
        "${folder}/${model.file}".source = pkgs.fetchurl {
          url = "https://huggingface.co/${model.repo}/resolve/main/${model.file}";
          inherit (model) sha256;
          name = "tokativ-${model.file}";
        };
        # The app counts a model as installed when this note sits beside the file.
        "${folder}/${stem}.json".text = builtins.toJSON {
          inherit (model) architecture sha256;
        };
      }
    ) cfg.prefetchModels
  );
in
{
  options.services.tokativ = {
    enable = lib.mkEnableOption "Tokativ, local dictation";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "tokativ.packages.\${system}.default";
      description = "The Tokativ package to run.";
    };

    settings = lib.mkOption {
      inherit (toml) type;
      default = { };
      example = lib.literalExpression ''
        {
          profile = "Casual";
          vad.silence_ms = 600;
          pill.margin = 64;
        }
      '';
      description = ''
        Written to {file}`$XDG_CONFIG_HOME/tokativ/config.toml`, or to
        {file}`~/Library/Application Support/tokativ/config.toml` on macOS. Left empty, the file stays
        under the settings window's control. When set, the settings window can still change
        it, and the next activation puts these values back.
      '';
    };

    prefetchModels = lib.mkOption {
      type = lib.types.listOf (
        lib.types.enum [
          "parakeet"
          "gigaam"
        ]
      );
      default = [ ];
      example = [ "parakeet" ];
      description = ''
        Models to fetch at build time and link into {file}`$XDG_DATA_HOME/tokativ/models`, or
        {file}`~/Library/Application Support/tokativ/models` on macOS, instead of downloading
        them from the setup window.
      '';
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      { home.packages = [ cfg.package ]; }

      (lib.mkIf isLinux {
        xdg.configFile = configFiles;
        xdg.dataFile = modelFiles;

        systemd.user.services.tokativ = {
          Unit = {
            Description = "Tokativ dictation daemon";
            PartOf = [ "graphical-session.target" ];
            After = [ "graphical-session.target" ];
          };
          Service = {
            ExecStart = "${lib.getExe cfg.package} daemon";
            Restart = "on-failure";
            RestartSec = 2;
          };
          Install.WantedBy = [ "graphical-session.target" ];
        };
      })

      # macOS keeps both the settings and the models under Application Support.
      (lib.mkIf isDarwin {
        home.file = lib.mapAttrs' (
          path: file: lib.nameValuePair "Library/Application Support/${path}" file
        ) (configFiles // modelFiles);

        launchd.agents.tokativ = {
          enable = true;
          config = {
            ProgramArguments = [
              (lib.getExe cfg.package)
              "daemon"
            ];
            RunAtLoad = true;
            KeepAlive.SuccessfulExit = false;
            ProcessType = "Interactive";
          };
        };
      })
    ]
  );
}
