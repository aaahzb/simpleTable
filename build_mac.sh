#!/bin/bash
# simpleTable macOS(arm64) 打包脚本：生成 dist/simpleTable.app
# 用法：bash build_mac.sh（需要 Mac 上有 python3.12+）
set -e
cd "$(dirname "$0")"
if [ ! -d venv ]; then
  python3 -m venv venv
fi
source venv/bin/activate
pip install -r requirements.txt pyinstaller -i https://mirrors.aliyun.com/pypi/simple/
# 注入版本信息：HEAD 有 v 开头 tag → release，否则 beta+commit
python make_version.py
pyinstaller --noconfirm --onefile --windowed --name simpleTable main.py
# 清理版本注入临时文件（不入库）
rm -f _build_info.py
echo "完成：dist/simpleTable.app"
