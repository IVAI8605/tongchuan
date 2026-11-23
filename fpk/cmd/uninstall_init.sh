#!/bin/bash
# 卸载前：先停服务，避免卸载时进程还在写
set -u
APP_DIR="${TRIM_APPDEST:-/var/apps/${TRIM_APPNAME:-tongchuan}}"
[ -x "${APP_DIR}/cmd/main" ] && "${APP_DIR}/cmd/main" stop
exit 0
