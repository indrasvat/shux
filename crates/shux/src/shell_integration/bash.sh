# shellcheck shell=bash
# shux shell integration for bash: tells shux where each command starts
# (OSC 133;C), how it ended (133;D;<exit>) and where the prompt starts
# (133;A), so `shux pane wait-command` needs no marker of yours.
#
# Add to ~/.bashrc, after ble.sh and starship (a hook added after this one
# would report its own status in place of the command's):
#   eval "$(shux shell-integration bash)"
__shux_osc133_install() {
  [[ $- == *i* ]] || return 0
  # A shell reading its rc has started no command. Inherited (allexport
  # exports it), the flag would make its first prompt end its parent's.
  __shux_osc133_ran=
  # Each hook below is added only if absent, so sourcing twice adds none. No
  # function guard: allexport exports functions, and a child would skip.
  if ((BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4))); then
    printf 'shux shell-integration: bash %s has no PS0; command boundaries need bash 4.4 or newer\n' \
      "$BASH_VERSION" >&2
    return 0
  fi
  # Leaves `$?` as the command left it for the hooks after this one; with
  # errexit on, a non-zero return would end the shell, so it returns 0.
  __shux_osc133_keep() {
    [[ $- == *e* ]] && return 0
    return "$1"
  }
  if [[ -n ${BLE_VERSION-} ]]; then
    # ble.sh draws its own marks after a command; report the end before them.
    __shux_osc133_start() { printf '\e]133;C\a'; }
    __shux_osc133_end() {
      local ec=$?
      printf '\e]133;D;%s\a' "$ec"
      __shux_osc133_keep "$ec"
    }
    __shux_osc133_prompt() {
      local ec=$?
      printf '\e]133;A\a'
      __shux_osc133_keep "$ec"
    }
    blehook PREEXEC!=__shux_osc133_start
    blehook POSTEXEC!=__shux_osc133_end
    blehook PRECMD!=__shux_osc133_prompt
    return 0
  fi
  # PS0 is expanded in this shell, not a subshell: the arithmetic marks that
  # this shell (by PID: allexport hands the flag to children) started the
  # command, and rides harmlessly as a C parameter.
  # shellcheck disable=SC2016  # expanded at each prompt, not now
  [[ ${PS0-} == *'133;C;shux='* ]] || PS0+='\e]133;C;shux=$((__shux_osc133_ran = $$))\a'
  __shux_osc133_precmd() {
    local ec=$?
    # Only a command this shell started ends here: a new shell's first
    # prompt (`bash`, `sudo -i` typed into this one) must not end its parent's.
    # With promptvars off PS0 cannot mark that, so every prompt reports.
    if [[ ${__shux_osc133_ran-} == "$$" ]] || ! shopt -q promptvars; then
      printf '\e]133;D;%s\a' "$ec"
      __shux_osc133_ran=
    fi
    printf '\e]133;A\a'
    __shux_osc133_keep "$ec"
  }
  [[ ${PROMPT_COMMAND[*]-} == *__shux_osc133_precmd* ]] && return 0
  # First, so it reads the command's status before any other hook runs.
  # An array runs every element only from bash 5.1; before, only the first.
  if ((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1] >= 501)) &&
    [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
    PROMPT_COMMAND=(__shux_osc133_precmd "${PROMPT_COMMAND[@]}")
  else
    # shellcheck disable=SC2178,SC2128  # this branch runs only when it is not an array
    PROMPT_COMMAND="__shux_osc133_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
  fi
}
__shux_osc133_install
unset -f __shux_osc133_install
