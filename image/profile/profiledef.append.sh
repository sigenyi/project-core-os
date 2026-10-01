# C.O.R.E. OS overrides. image/build-iso.sh appends this to archiso releng's
# profiledef.sh, so later assignments here win while boot modes and other
# archiso-version-specific settings stay as releng defines them.
# shellcheck shell=bash disable=SC2034

iso_name="core-os"
iso_label="CORE_$(date --date="@${SOURCE_DATE_EPOCH:-$(date +%s)}" +%Y%m)"
iso_publisher="C.O.R.E. OS <https://github.com/sigenyi/project-core-os>"
iso_application="C.O.R.E. OS live medium"
iso_version="$(date --date="@${SOURCE_DATE_EPOCH:-$(date +%s)}" +%Y.%m.%d)"

file_permissions+=(
    ["/etc/shadow"]="0:0:400"
    ["/etc/gshadow"]="0:0:400"
    ["/etc/core/guardian.toml"]="0:0:644"
    ["/etc/core/agent.toml"]="0:0:644"
    ["/etc/core/inference.env"]="0:0:644"
    ["/home/core"]="1000:1000:750"
    ["/home/core/.hushlogin"]="1000:1000:644"
    ["/usr/bin/core-shell"]="0:0:755"
    ["/usr/bin/core-guardian"]="0:0:755"
    ["/usr/bin/core-sensed"]="0:0:755"
    ["/usr/bin/core-ctl"]="0:0:755"
)
