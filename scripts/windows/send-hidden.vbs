' Tongchuan hidden sender for Explorer context menu / SendTo.
' Usage: wscript.exe //B //Nologo send-hidden.vbs <tc.exe> <to> <peer-fp> <path> [path...]
Option Explicit

Dim shell, fso, tc, target, fp, i, cmd
If WScript.Arguments.Count < 3 Then WScript.Quit 2

Set shell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
tc = WScript.Arguments(0)
target = WScript.Arguments(1)
fp = WScript.Arguments(2)
' Run tc.exe from its own app directory so its default config.toml/identity is found.
shell.CurrentDirectory = fso.GetParentFolderName(tc)

cmd = Chr(34) & tc & Chr(34) & " send --to " & target
If Len(fp) > 0 Then cmd = cmd & " --peer-fp " & fp
For i = 3 To WScript.Arguments.Count - 1
    cmd = cmd & " --dir " & Chr(34) & WScript.Arguments(i) & Chr(34)
Next
cmd = cmd & " --retry"

' 0 = hidden window; False = do not wait, let Explorer stay responsive.
shell.Run cmd, 0, False
