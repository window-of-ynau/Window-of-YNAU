; 云农之窗 Windows 安装包（Inno Setup 6）
;
; 选 Inno Setup 而不是 WiX 的理由：
;   - 单文件 .iss + 一条 ISCC 命令，CI 里 `choco install innosetup` 即可跑，无需 MSBuild 工程与
;     WiX Toolset 版本矩阵；WiX v4 的 .wxs/.wixproj 维护成本对我们这个规模的桌面端过重；
;   - 内置中文界面、开始菜单/桌面快捷方式、卸载程序，全部是声明式配置；
;   - 卸载脚本里能写 Pascal [Code] 段——"保留用户数据并提示位置"这条要求，用 Inno 的
;     CurUninstallStepChanged 三行就能做到。
;
; 由 scripts/build-windows-installer.ps1 调用：
;   ISCC.exe /DMyAppVersion=0.1.0 /DSourceDir=<Release 目录> /DOutputDir=<dist>
;
; 约定（与 scripts/build-windows-app.ps1 一致）：yunong.exe、yunong_core.dll、
; flutter_windows.dll 必须在**同一个目录**，data\ 整个子目录一起带上。

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "app\build\windows\x64\runner\Release"
#endif
#ifndef OutputDir
  #define OutputDir "dist"
#endif

[Setup]
; AppId 一旦发布就不能改（Windows 用它识别升级/卸载）；这里固定成项目专属 GUID。
AppId={{6F2B1A4C-7D3E-4A62-9B5C-2E8F1D4A7C31}
AppName=云农之窗
AppVersion={#MyAppVersion}
AppVerName=云农之窗 {#MyAppVersion}
AppPublisher=Window of YNAU
AppPublisherURL=https://github.com/window-of-ynau/Window-of-YNAU
AppSupportURL=https://github.com/window-of-ynau/Window-of-YNAU/issues
DefaultDirName={autopf}\WindowOfYNAU
DefaultGroupName=云农之窗
DisableProgramGroupPage=yes
OutputDir={#OutputDir}
OutputBaseFilename=WindowOfYNAU-{#MyAppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\yunong.exe
; 暂无代码签名证书：明确不签卸载器，避免 ISCC 去找签名工具。
SignedUninstaller=no
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog

; 不加载第三方简体中文 .isl：官方 Inno Setup 6 不附带 ChineseSimplified.isl（社区补丁），
; 在 CI 里引它会 "Couldn't open include file" 直接失败。向导按钮用默认英文，界面字符串
; （AppName / 快捷方式 / 卸载提示）本身都是中文，用户感知一致。要全中文向导可自行放入
; Languages\ChineseSimplified.isl 并在本机 ISCC 里加 [Languages] 段。

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加任务："; Flags: unchecked

[Files]
; 整个 Release 目录（含 data\ 与全部 DLL）——DLL 必须与 exe 同目录，不能拆。
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[Icons]
Name: "{group}\云农之窗"; Filename: "{app}\yunong.exe"
Name: "{group}\卸载云农之窗"; Filename: "{uninstallexe}"
Name: "{autodesktop}\云农之窗"; Filename: "{app}\yunong.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\yunong.exe"; Description: "启动云农之窗"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; 只删安装目录本身，**不碰** %LOCALAPPDATA%\yunong（SQLite 数据库 + DPAPI 密文都在那里）。
Type: filesandordirs; Name: "{app}"

[Code]
// 卸载完成页明确告知数据位置：卸载不删数据，但用户必须知道去哪清。
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    DataDir := ExpandConstant('{localappdata}\yunong');
    // 静默卸载（CI 验证 / 脚本化）不能弹窗，否则会卡住
    if not UninstallSilent then
    MsgBox('云农之窗已卸载。' + #13#10#13#10 +
           '你的本地数据**仍保留**在：' + #13#10 + DataDir + #13#10#13#10 +
           '（包含课表数据库、记忆/凭据的 DPAPI 密文、设置）。' + #13#10 +
           '如需彻底清除，手动删除上述目录即可；重新安装会自动沿用这些数据。',
           mbInformation, MB_OK);
  end;
end;
