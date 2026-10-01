//! Opt-in shell integrations for daemon-private ctmux awareness reports.

/// Returns source code for a supported interactive shell.
///
/// The snippets intentionally become inert outside an ctmux session: ctmuxd
/// supplies `CTMUX_SHELL_STATE_PIPE` only to children it owns.
#[must_use]
pub fn init_script(shell: Shell) -> &'static str {
  match shell {
    Shell::Zsh => ZSH_INIT,
    Shell::Bash => BASH_INIT,
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
  Bash,
  Zsh,
}

const ZSH_INIT: &str = r#"# ctmux shell-awareness integration v2 for zsh.
# Add this once to ~/.zshrc:
#   eval "$(ctmux shell init zsh)"
if [[ -o interactive && -n ${CTMUX_SHELL_STATE_PIPE-} && -z ${__CTMUX_SHELL_AWARENESS-} ]]; then
  # The FIFO pathname is capability-like data. Keep it only in this shell and
  # open/write/close it for each report so executed commands inherit neither
  # its environment variable nor a usable writer descriptor.
  typeset -g __ctmux_shell_state_pipe="$CTMUX_SHELL_STATE_PIPE"
  typeset +x __ctmux_shell_state_pipe
  unset CTMUX_SHELL_STATE_PIPE
  typeset -g __CTMUX_SHELL_AWARENESS=1
  if [[ -n $__ctmux_shell_state_pipe ]]; then
    autoload -Uz add-zsh-hook add-zle-hook-widget
    zmodload zsh/datetime 2>/dev/null
    typeset -gF __ctmux_shell_state_last_report=0

    function __ctmux_shell_state_emit() {
      local prompt_phase=$1
      local command_line_present=$2
      local command_line=${3-}
      local cursor_scalar_offset=${4-}
      # The protocol bounds text in UTF-8 bytes, while zsh normally counts
      # characters. The C locale makes `${#value}` use byte length.
      local LC_ALL=C

      # Do not truncate an editable line: omitting it is safer than presenting
      # a misleading prefix. The daemon also enforces this bound.
      if (( ${#command_line} > 4096 )); then
        command_line_present=0
        command_line=''
        cursor_scalar_offset=''
      fi

      builtin printf '%s\0%s\0%s\0%s\0%s\0%s\0%s\0%s\0%s\0' \
        ctmux-shell-v2 zsh 2 cwd,command_line,cursor,prompt_phase,running_command \
        "$PWD" "$prompt_phase" "$command_line_present" "$command_line" "$cursor_scalar_offset" \
        >"$__ctmux_shell_state_pipe" 2>/dev/null
    }

    function __ctmux_shell_state_has_safe_running_command() {
      local command_line=$1
      if [[ -z $command_line || $command_line == *[[:cntrl:]]* ]]; then
        return 1
      fi
      # This is deliberately after the control check: use the caller locale
      # for the Unicode character class, then use C only for byte length.
      local LC_ALL=C
      (( ${#command_line} <= 256 ))
    }

    function __ctmux_shell_state_precmd() {
      __ctmux_shell_state_emit at_prompt 0
    }

    function __ctmux_shell_state_preexec() {
      # The v2 active-text fields are phase-exclusive. The daemon retains this
      # title summary only when it is a bounded, non-control-character string.
      if __ctmux_shell_state_has_safe_running_command "$1"; then
        __ctmux_shell_state_emit running 1 "$1" ''
      else
        # Still publish the running phase so the daemon clears any prior
        # editable buffer. Only the optional title preview is omitted.
        __ctmux_shell_state_emit running 0 '' ''
      fi
    }

    function __ctmux_shell_state_line_pre_redraw() {
      local now=${EPOCHREALTIME:-0}
      if (( now > 0 && now - __ctmux_shell_state_last_report < 0.1 )); then
        return 0
      fi
      __ctmux_shell_state_last_report=$now
      __ctmux_shell_state_emit editing 1 "$BUFFER" "$CURSOR"
    }

    function __ctmux_shell_state_line_finish() {
      __ctmux_shell_state_emit at_prompt 0
    }

    add-zsh-hook precmd __ctmux_shell_state_precmd
    add-zsh-hook preexec __ctmux_shell_state_preexec
    add-zle-hook-widget line-pre-redraw __ctmux_shell_state_line_pre_redraw
    add-zle-hook-widget line-finish __ctmux_shell_state_line_finish
  fi
fi
"#;

const BASH_INIT: &str = r#"# ctmux shell-awareness integration v2 for bash.
# Add this once to ~/.bashrc:
#   eval "$(ctmux shell init bash)"
if [[ $- == *i* && -n ${CTMUX_SHELL_STATE_PIPE:-} && -z ${__CTMUX_SHELL_AWARENESS:-} ]]; then
  # Do not pass the private reporter pathname or a writer descriptor into
  # commands launched by this shell.
  __ctmux_shell_state_pipe=$CTMUX_SHELL_STATE_PIPE
  declare +x __ctmux_shell_state_pipe
  unset CTMUX_SHELL_STATE_PIPE
  __CTMUX_SHELL_AWARENESS=1
  if [[ -n ${__ctmux_shell_state_pipe:-} ]]; then
    __ctmux_shell_state_bash_emit() {
      builtin printf '%s\0%s\0%s\0%s\0%s\0%s\0%s\0%s\0%s\0' \
        ctmux-shell-v2 bash 2 cwd,prompt_phase \
        "$PWD" at_prompt 0 '' '' >"$__ctmux_shell_state_pipe" 2>/dev/null
    }

    __ctmux_shell_state_bash_precmd() {
      __ctmux_shell_state_bash_emit
    }

    if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a "* ]]; then
      PROMPT_COMMAND=(__ctmux_shell_state_bash_precmd "${PROMPT_COMMAND[@]}")
    elif [[ -n ${PROMPT_COMMAND:-} ]]; then
      PROMPT_COMMAND="__ctmux_shell_state_bash_precmd;${PROMPT_COMMAND}"
    else
      PROMPT_COMMAND=__ctmux_shell_state_bash_precmd
    fi
  fi
fi
"#;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn zsh_integration_advertises_live_editing_capabilities() {
    let script = init_script(Shell::Zsh);
    assert!(script.contains("ctmux-shell-v2 zsh 2"));
    assert!(script.contains("cwd,command_line,cursor,prompt_phase,running_command"));
    assert!(script.contains("function __ctmux_shell_state_has_safe_running_command"));
    assert!(script.contains("local LC_ALL=C"));
    assert!(script.contains("(( ${#command_line} <= 256 ))"));
    assert!(script.contains("__ctmux_shell_state_emit running 0 '' ''"));
    assert!(script.contains("line-pre-redraw"));
    assert!(script.contains("CTMUX_SHELL_STATE_PIPE"));
    assert!(script.contains("unset CTMUX_SHELL_STATE_PIPE"));
    assert!(script.contains("typeset +x __ctmux_shell_state_pipe"));
    assert!(!script.contains("exec {__ctmux_shell_state_fd}"));
  }

  #[test]
  fn bash_integration_does_not_claim_a_live_edit_buffer() {
    let script = init_script(Shell::Bash);
    assert!(script.contains("ctmux-shell-v2 bash 2"));
    assert!(script.contains("cwd,prompt_phase"));
    assert!(!script.contains("cwd,command_line,cursor,prompt_phase"));
    assert!(script.contains("unset CTMUX_SHELL_STATE_PIPE"));
    assert!(script.contains("declare +x __ctmux_shell_state_pipe"));
    assert!(!script.contains("exec 9>"));
  }

  #[cfg(unix)]
  #[test]
  fn bash_integration_does_not_pass_the_reporter_path_to_an_executed_child() {
    let output = std::process::Command::new("bash")
      .args([
        "--noprofile",
        "--norc",
        "-i",
        "-c",
        "set -a; eval \"$1\"; command sh -c 'test -z \"${CTMUX_SHELL_STATE_PIPE+x}\" && test -z \"${__ctmux_shell_state_pipe+x}\"'",
        "--",
        init_script(Shell::Bash),
      ])
      .env("CTMUX_SHELL_STATE_PIPE", "/dev/null")
      .output()
      .expect("bash should execute its integration");
    assert!(
      output.status.success(),
      "bash child inherited CTMUX_SHELL_STATE_PIPE: {}",
      String::from_utf8_lossy(&output.stderr)
    );
  }
}
