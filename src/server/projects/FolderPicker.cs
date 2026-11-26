using System;
using System.Runtime.InteropServices;

public static class LyraFolderPicker
{
    private const uint PickFolders = 0x20;
    private const uint ForceFileSystem = 0x40;
    private const uint FileSystemPath = 0x80058000;
    private const int Cancelled = unchecked((int)0x800704C7);

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [ComImport, Guid("DC1C5A9C-E88A-4DDE-A5A1-60F82A20AEF7")]
    private class FileOpenDialog { }

    // Members are in the order defined by the Win32 IFileDialog interface.
    [ComImport, Guid("42F85136-DB7E-439C-85F1-E4075D135FC8")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IFileDialog
    {
        [PreserveSig] int Show(IntPtr owner);
        void SetFileTypes(uint count, IntPtr specifications);
        void SetFileTypeIndex(uint index);
        uint GetFileTypeIndex();
        uint Advise(IntPtr events);
        void Unadvise(uint cookie);
        void SetOptions(uint options);
        uint GetOptions();
        void SetDefaultFolder(IShellItem item);
        void SetFolder(IShellItem item);
        IShellItem GetFolder();
        IShellItem GetCurrentSelection();
        void SetFileName([MarshalAs(UnmanagedType.LPWStr)] string name);
        IntPtr GetFileName();
        void SetTitle([MarshalAs(UnmanagedType.LPWStr)] string title);
        void SetOkButtonLabel([MarshalAs(UnmanagedType.LPWStr)] string text);
        void SetFileNameLabel([MarshalAs(UnmanagedType.LPWStr)] string label);
        IShellItem GetResult();
    }

    [ComImport, Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItem
    {
        IntPtr BindToHandler(IntPtr bindContext, ref Guid handler, ref Guid iid);
        IShellItem GetParent();
        IntPtr GetDisplayName(uint nameType);
    }

    public static string Choose()
    {
        var dialog = (IFileDialog)new FileOpenDialog();
        try
        {
            dialog.SetOptions(dialog.GetOptions() | PickFolders | ForceFileSystem);
            dialog.SetTitle("Lyra");
            int result = dialog.Show(GetForegroundWindow());
            if (result == Cancelled)
                return null;
            Marshal.ThrowExceptionForHR(result);
            var folder = dialog.GetResult();
            try
            {
                IntPtr path = folder.GetDisplayName(FileSystemPath);
                try { return Marshal.PtrToStringUni(path); }
                finally { Marshal.FreeCoTaskMem(path); }
            }
            finally { Marshal.ReleaseComObject(folder); }
        }
        finally { Marshal.ReleaseComObject(dialog); }
    }
}
