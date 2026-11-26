#!/bin/bash
# 安装后：建数据目录、给二进制可执行位，并记录安装向导里用户填的「接收子目录名」。
set -u
APP_DIR="${TRIM_APPDEST:-/var/apps/${TRIM_APPNAME:-tongchuan}}"
DATA_DIR="${TRIM_PKGVAR:-/vol1/@appdata/${TRIM_APPNAME:-tongchuan}}"
mkdir -p "${DATA_DIR}" || exit 1
# 升级迁移：数据目录可能是旧版以 root 创建，交给专用包用户，避免新版本(package 运行)读不到
chown -R "${TRIM_APPNAME:-tongchuan}" "${DATA_DIR}" 2>/dev/null || true

# 只取一段安全的名字（去掉路径分隔符/..），留空则由 main.sh 用默认「通传接收」。
SUB="${wizard_subdir:-}"
SUB="$(printf '%s' "$SUB" | tr -d '\r' | sed 's#[/\\].*##; s/^[[:space:]]*//; s/[[:space:]]*$//')"
case "$SUB" in
    ""|"."|"..") echo "tongchuan: 接收子目录用默认「通传接收」" ;;
    *) printf '%s\n' "$SUB" > "${DATA_DIR}/.install-subdir"; echo "tongchuan: 接收子目录设为 $SUB" ;;
esac

# 记录安装向导选的信任模式（缺省 closed）
TM="${wizard_trust_mode:-closed}"
[ "$TM" = "open" ] || TM="closed"
printf '%s\n' "$TM" > "${DATA_DIR}/.install-trust"
echo "tongchuan: 信任模式 = $TM"

chmod 755 "${APP_DIR}/bin/tc" 2>/dev/null
echo "tongchuan installed: ${APP_DIR} ; data: ${DATA_DIR}"
exit 0
