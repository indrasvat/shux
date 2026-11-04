# shux shell integration for zsh: tells shux where each command starts
# (OSC 133;C), how it ended (133;D;<exit>) and where the prompt starts
# (133;A), so `shux pane wait-command` needs no marker of yours.
#
# Add to ~/.zshrc, after any prompt framework:
#   eval "$(shux shell-integration zsh)"
__shux_osc133_install() {
  [[ -o interactive ]] || return 0
  # A shell reading its rc has started no command. Inherited (allexport
  # exports it), the flag would make its first prompt end its parent's.
  typeset -g __shux_osc133_ran=
  (( ${+functions[__shux_osc133_precmd]} )) && return 0
  __shux_osc133_preexec() {
    typeset -g __shux_osc133_ran=1
    print -n '\e]133;C\a'
  }
  __shux_osc133_precmd() {
    local ec=$?
    # Only a command this shell started ends here: a new shell's first
    # prompt (`zsh`, `sudo -i` typed into this one) must not end its parent's.
    if [[ -n ${__shux_osc133_ran-} ]]; then
      print -n "\e]133;D;${ec}\a"
      __shux_osc133_ran=
    fi
    print -n '\e]133;A\a'
    # With ERR_EXIT on, a non-zero return would end the shell.
    [[ -o errexit ]] && return 0
    return $ec
  }
  # First, so they run before any other hook touches the status.
  preexec_functions=(__shux_osc133_preexec $preexec_functions)
  precmd_functions=(__shux_osc133_precmd $precmd_functions)
}
__shux_osc133_install
unset -f __shux_osc133_install
