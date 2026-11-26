#!/bin/bash
# 安装后：建数据目录、给二进制可执行位，并记录安装向导里用户选的「接收目录」。
set -u
APP_DIR="${TRIM_APPDEST:-/var/apps/${TRIM_APPNAME:-tongchuan}}"
DATA_DIR="${TRIM_PKGVAR:-/vol1/@appdata/${TRIM_APPNAME:-tongchuan}}"
mkdir -p "${DATA_DIR}" || exit 1

# 安装向导字段 wizard_destination 会以环境变量传入。只接受绝对路径，留空则用默认（应用数据目录 received/）。
DEST="${wizard_destination:-}"
case "$DEST" in
    /*)
        mkdir -p "$DEST" 2>/dev/null
        printf '%s\n' "$DEST" > "${DATA_DIR}/.install-destination"
        echo "tongchuan: 接收目录设为 $DEST"
        ;;
    "")
        echo "tongchuan: 未指定接收目录，使用默认（应用数据目录 received/）"
        ;;
    *)
        echo "tongchuan: 接收目录需为绝对路径，忽略「$DEST」，使用默认"
        ;;
esac

chmod 755 "${APP_DIR}/bin/tc" 2>/dev/null
echo "tongchuan installed: ${APP_DIR} ; data: ${DATA_DIR}"
exit 0
