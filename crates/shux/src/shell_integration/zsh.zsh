# shux shell integration for zsh: tells shux where each command starts
# (OSC 133;C), how it ended (133;D;<exit>) and where the prompt starts
# (133;A), so `shux pane wait-command` needs no marker of yours.
#
# Add to ~/.zshrc, after any prompt framework:
#   eval "$(shux shell-integration zsh)"
__shux_osc133_install() {
  [[ -o interactive ]] || return 0
  (( ${+__shux_osc133} )) && return 0
  typeset -g __shux_osc133=1
  __shux_osc133_preexec() { print -n '\e]133;C\a' }
  __shux_osc133_precmd() {
    local ec=$?
    print -n "\e]133;D;${ec}\a\e]133;A\a"
    return $ec
  }
  # First, so they run before any other hook touches the status.
  preexec_functions=(__shux_osc133_preexec $preexec_functions)
  precmd_functions=(__shux_osc133_precmd $precmd_functions)
}
__shux_osc133_install
unset -f __shux_osc133_install
