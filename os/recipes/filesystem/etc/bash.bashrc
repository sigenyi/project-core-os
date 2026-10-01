# System-wide bash settings for interactive shells.
[[ $- != *i* ]] && return
shopt -s checkwinsize histappend
HISTCONTROL=ignoredups
PS1='\u@\h:\w\$ '
alias ls='ls --color=auto'
