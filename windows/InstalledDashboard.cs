using System;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

namespace LyraTray
{
    internal static class InstalledDashboard
    {
        private const string AppsFolder = "shell:::{4234d49b-0245-4df3-b780-3893943456e1}";
        private static readonly PropertyKey AppUserModelId = new PropertyKey
        {
            Format = new Guid("9f4c2855-9f79-4b39-a8d0-e1d42de1d5f3"),
            Id = 5,
        };

        public static string AppName(string dashboard)
        {
            // Chromium doubles the SHA-256 over the manifest ID, then encodes
            // the first 16 bytes with the alphabet a-p. The manifest ID is "/".
            using (var sha = SHA256.Create())
            {
                byte[] hash = sha.ComputeHash(sha.ComputeHash(Encoding.UTF8.GetBytes(dashboard)));
                var name = new StringBuilder("_crx_");
                for (int i = 0; i < 16; i++)
                {
                    name.Append((char)('a' + (hash[i] >> 4)));
                    name.Append((char)('a' + (hash[i] & 15)));
                }
                return name.ToString();
            }
        }

        private static bool Matches(string windowsId, string appName)
        {
            return windowsId != null &&
                Array.Exists(windowsId.Split('.'), part => string.Equals(part, appName, StringComparison.OrdinalIgnoreCase));
        }

        public static IntPtr FindWindow(string appName)
        {
            IntPtr result = IntPtr.Zero;
            EnumWindows((window, data) =>
            {
                if (!IsWindowVisible(window) || !Matches(ReadWindowAppId(window), appName)) return true;
                result = window;
                return false;
            }, IntPtr.Zero);
            return result;
        }

        public static bool TryLaunch(string appName)
        {
            try
            {
                Type shellType = Type.GetTypeFromProgID("Shell.Application");
                if (shellType == null) return false;
                object shell = Activator.CreateInstance(shellType);
                object folder = Call(shell, "NameSpace", BindingFlags.InvokeMethod, AppsFolder);
                if (folder == null) return false;
                object items = Call(folder, "Items", BindingFlags.InvokeMethod);
                if (items == null) return false;
                int count = (int)Call(items, "Count", BindingFlags.GetProperty);
                for (int i = 0; i < count; i++)
                {
                    object item = Call(items, "Item", BindingFlags.InvokeMethod, i);
                    if (item == null || !Matches(Call(item, "Path", BindingFlags.GetProperty) as string, appName)) continue;
                    Call(item, "InvokeVerb", BindingFlags.InvokeMethod | BindingFlags.OptionalParamBinding, Type.Missing);
                    return true;
                }
            }
            catch (COMException) { }
            catch (TargetInvocationException) { }
            catch (MissingMemberException) { }
            return false;
        }

        private static object Call(object target, string member, BindingFlags flags, params object[] args)
        {
            return target.GetType().InvokeMember(member, flags, null, target, args);
        }

        private static string ReadWindowAppId(IntPtr window)
        {
            var interfaceId = typeof(IPropertyStore).GUID;
            IPropertyStore properties;
            if (SHGetPropertyStoreForWindow(window, ref interfaceId, out properties) < 0 || properties == null)
                return null;

            try
            {
                var key = AppUserModelId;
                PropertyValue value;
                if (properties.GetValue(ref key, out value) < 0) return null;
                try
                {
                    const ushort WideString = 31; // VT_LPWSTR
                    return value.Type == WideString ? Marshal.PtrToStringUni(value.Data) : null;
                }
                finally { PropVariantClear(ref value); }
            }
            finally { Marshal.ReleaseComObject(properties); }
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct PropertyKey
        {
            public Guid Format;
            public uint Id;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct PropertyValue
        {
            public ushort Type;
            private ushort reserved1, reserved2, reserved3;
            public IntPtr Data;
            private IntPtr extra;
        }

        [ComImport, Guid("886d8eeb-8cf2-4446-8d02-cdba1dbdcf99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IPropertyStore
        {
            [PreserveSig] int GetCount(out uint count);
            [PreserveSig] int GetAt(uint index, out PropertyKey key);
            [PreserveSig] int GetValue(ref PropertyKey key, out PropertyValue value);
        }

        private delegate bool WindowCallback(IntPtr window, IntPtr data);

        [DllImport("user32.dll")]
        private static extern bool EnumWindows(WindowCallback callback, IntPtr data);

        [DllImport("user32.dll")]
        private static extern bool IsWindowVisible(IntPtr window);

        [DllImport("shell32.dll")]
        private static extern int SHGetPropertyStoreForWindow(IntPtr window, ref Guid interfaceId, out IPropertyStore properties);

        [DllImport("ole32.dll")]
        private static extern int PropVariantClear(ref PropertyValue value);
    }
}
