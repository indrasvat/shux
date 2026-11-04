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
  [[ -n ${__shux_osc133-} ]] && return 0
  if ((BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4))); then
    printf 'shux shell-integration: bash %s has no PS0; command boundaries need bash 4.4 or newer\n' \
      "$BASH_VERSION" >&2
    return 0
  fi
  __shux_osc133=1
  if [[ -n ${BLE_VERSION-} ]]; then
    # ble.sh draws its own marks after a command; report the end before them.
    __shux_osc133_start() { printf '\e]133;C\a'; }
    __shux_osc133_end() {
      local ec=$?
      printf '\e]133;D;%s\a' "$ec"
      return "$ec"
    }
    __shux_osc133_prompt() {
      local ec=$?
      printf '\e]133;A\a'
      return "$ec"
    }
    blehook PREEXEC+=__shux_osc133_start
    blehook POSTEXEC+=__shux_osc133_end
    blehook PRECMD+=__shux_osc133_prompt
    return 0
  fi
  PS0+='\e]133;C\a'
  __shux_osc133_precmd() {
    local ec=$?
    printf '\e]133;D;%s\a\e]133;A\a' "$ec"
    return "$ec"
  }
  # First, so it reads the command's status before any other hook runs.
  if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
    PROMPT_COMMAND=(__shux_osc133_precmd "${PROMPT_COMMAND[@]}")
  else
    # shellcheck disable=SC2178,SC2128  # this branch runs only when it is not an array
    PROMPT_COMMAND="__shux_osc133_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
  fi
}
__shux_osc133_install
unset -f __shux_osc133_install
