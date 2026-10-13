@echo off
rem simpleTable Windows 打包脚本：生成 dist\simpleTable.exe
set ROOT=%~dp0
set VENV=%ROOT%venv
rem 注入版本信息：HEAD 有 v 开头 tag → release，否则 beta+commit
"%VENV%\python.exe" "%ROOT%make_version.py"
"%VENV%\Scripts\pip.exe" install pyinstaller -i https://mirrors.aliyun.com/pypi/simple/
"%VENV%\Scripts\pyinstaller.exe" --noconfirm --onefile --windowed --name simpleTable "%ROOT%main.py"
rem 清理版本注入临时文件（不入库）
del /q "%ROOT%_build_info.py" 2>nul
echo.
echo 完成：%ROOT%dist\simpleTable.exe
pause
