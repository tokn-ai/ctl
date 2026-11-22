$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class CtlupShellItem {
  [DllImport("shell32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
  private static extern int SHParseDisplayName(string path, IntPtr context, out IntPtr item, uint attributes, IntPtr found);

  [DllImport("shell32.dll", ExactSpelling = true)]
  private static extern uint ILGetSize(IntPtr item);

  public static byte[] Read(string path) {
    IntPtr item;
    int result = SHParseDisplayName(path, IntPtr.Zero, out item, 0, IntPtr.Zero);
    Marshal.ThrowExceptionForHR(result);
    if (item == IntPtr.Zero) { throw new InvalidOperationException("Windows returned no Shell item."); }
    try {
      uint size = ILGetSize(item);
      if (size < 4 || size > UInt16.MaxValue) { throw new InvalidOperationException("Invalid Shell item ID list size."); }
      byte[] bytes = new byte[size];
      Marshal.Copy(item, bytes, 0, (int)size);
      return bytes;
    } finally {
      Marshal.FreeCoTaskMem(item);
    }
  }
}
'@
$bytes = [CtlupShellItem]::Read($env:CTLUP_SHORTCUT_TARGET)
$stdout = [Console]::OpenStandardOutput()
$stdout.Write($bytes, 0, $bytes.Length)
$stdout.Flush()
