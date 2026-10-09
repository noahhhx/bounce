{ pkgs, lib, config, ... }:

{
  # https://devenv.sh/packages/
  packages = [ pkgs.git ];

  # https://devenv.sh/languages/rust/
  languages.rust = {
    enable = true;
    channel = "stable";
    components = [
      "rustc"
      "cargo"
      "clippy"
      "rustfmt"
      "rust-analyzer"
    ];
  };

  # https://devenv.sh/scripts/
  scripts.hello.exec = "echo hello from devenv";

  # https://devenv.sh/git-hooks/
  git-hooks.hooks = {
    rustfmt = {
      enable = true;
      entry = "cargo fmt --all -- --check";
      pass_filenames = false;
      files = "\\.rs$";
      language = "system";
    };
    clippy = {
      enable = true;
      entry = "cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic";
      pass_filenames = false;
      files = "\\.rs$";
      language = "system";
    };
  };

  # https://devenv.sh/tests/
  enterTest = ''
    cargo clippy --version
    cargo fmt --version
  '';
}
