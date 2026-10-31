@echo off
chcp 936 >nul

REM ============================================================
REM  v0 Demo - 一键启动服务并穿透公网（单窗口，仅本地实验用）
REM  双击本文件即可启动
REM ============================================================

REM 本脚本会带 __serve 参数重新调用自身，用于在同一个窗口里
REM 依次启动 uvicorn 和 cpolar。
REM 这样写的原因：cpolar 的路径含空格，直接塞进 cmd /k "..." 的
REM 引号串里会解析失败，放进批处理里执行则没有引号嵌套问题。
if /i "%~1"=="__serve" goto :serve

setlocal
cd /d "%~dp0"

echo ========================================
echo    云农之窗 - 一键启动服务并穿透公网
echo ========================================
echo.

if not exist ".venv\Scripts\activate.bat" (
    echo [错误] 未找到 .venv 虚拟环境，请先在项目根目录创建。
    echo        当前目录: %CD%
    echo.
    pause
    exit /b 1
)

echo [1/2] 正在打开服务窗口（uvicorn + cpolar 合并显示）...
start "云农之窗-服务 (uvicorn + cpolar)" cmd /k call "%~f0" __serve

echo.
echo ========================================
echo   服务已启动！
echo ========================================
echo   本地地址: http://127.0.0.1:8000/
echo   公网地址: 看服务窗口里 cpolar 打印的 Forwarding 那一行
echo   停止服务: 直接关闭那个服务窗口即可
echo ========================================
echo.
echo 提示: 公网地址每次启动可能不同，发给别人前先确认一下。
echo.
pause
exit /b 0


:serve
REM ---------- 以下是单窗口内的实际启动逻辑 ----------
chcp 936 >nul
cd /d "%~dp0"
title 云农之窗-服务 (uvicorn + cpolar)

echo ========================================
echo    云农之窗 - 服务窗口
echo ========================================
echo.

echo [1/2] 激活虚拟环境...
call ".venv\Scripts\activate.bat"
if errorlevel 1 (
    echo [错误] 激活虚拟环境失败，请检查 .venv。
    echo.
    pause
    exit /b 1
)

echo [2/2] 启动后端: uvicorn main:app --reload
echo       后端日志会打印在本窗口，和 cpolar 输出合并显示。
echo.
start "" /b uvicorn main:app --reload

echo 等待 3 秒，让后端先就绪...
ping -n 4 127.0.0.1 >nul
echo.

echo 启动内网穿透: cpolar http 8000
echo 关闭本窗口即可同时停止后端与穿透。
echo ----------------------------------------
echo.

cpolar http 8000

echo.
echo ----------------------------------------
echo cpolar 已退出，正在停止后端...
taskkill /f /im uvicorn.exe >nul 2>&1
echo 全部服务已停止，按任意键关闭本窗口。
pause >nul
exit /b 0
