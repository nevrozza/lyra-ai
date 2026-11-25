using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

namespace LyraTray
{
    internal sealed class ProcessJob : IDisposable
    {
        private static readonly IntPtr InvalidHandle = new IntPtr(-1);
        private const int JobObjectExtendedLimitInformation = 9;
        private const uint KillOnJobClose = 0x2000;
        private const uint GenericRead = 0x80000000, GenericWrite = 0x40000000;
        private const uint FileShareReadWrite = 3, CreateAlways = 2, OpenExisting = 3, FileAttributeNormal = 0x80;
        private const int StartfUseStdHandles = 0x100;
        private const uint CreateSuspended = 0x4, CreateNoWindow = 0x08000000, WaitTimeout = 0x102;
        private IntPtr handle;

        public ProcessJob()
        {
            handle = CreateJobObjectW(IntPtr.Zero, null);
            if (handle == IntPtr.Zero) throw new Win32Exception();

            var limits = new ExtendedLimits();
            limits.Basic.LimitFlags = KillOnJobClose;
            if (!SetInformationJobObject(handle, JobObjectExtendedLimitInformation, ref limits, (uint)Marshal.SizeOf(typeof(ExtendedLimits))))
            {
                var error = new Win32Exception();
                Dispose();
                throw error;
            }
        }

        public Child Start(string executable, string arguments, string directory, string log)
        {
            var security = new SecurityAttributes { Size = Marshal.SizeOf(typeof(SecurityAttributes)), Inherit = 1 };
            IntPtr output = CreateFileW(log, GenericWrite, FileShareReadWrite, ref security, CreateAlways, FileAttributeNormal, IntPtr.Zero);
            if (output == InvalidHandle) throw new Win32Exception();
            IntPtr input = InvalidHandle;
            try
            {
                input = CreateFileW("NUL", GenericRead, FileShareReadWrite, ref security, OpenExisting, FileAttributeNormal, IntPtr.Zero);
                if (input == InvalidHandle) throw new Win32Exception();
                var startup = new StartupInfo
                {
                    Size = Marshal.SizeOf(typeof(StartupInfo)), Flags = StartfUseStdHandles,
                    Input = input, Output = output, Error = output
                };
                var command = new StringBuilder("\"" + executable + "\" " + arguments);
                ProcessInfo process;
                // Assign the suspended process first, so even its earliest children belong to the job.
                if (!CreateProcessW(executable, command, IntPtr.Zero, IntPtr.Zero, true, CreateSuspended | CreateNoWindow,
                    IntPtr.Zero, directory, ref startup, out process)) throw new Win32Exception();
                try
                {
                    if (!AssignProcessToJobObject(handle, process.Process) || ResumeThread(process.Thread) == uint.MaxValue)
                    {
                        var error = new Win32Exception();
                        TerminateProcess(process.Process, 1);
                        CloseHandle(process.Process);
                        throw error;
                    }
                    return new Child(process.Process);
                }
                finally { CloseHandle(process.Thread); }
            }
            finally
            {
                if (input != InvalidHandle) CloseHandle(input);
                CloseHandle(output);
            }
        }

        public void Dispose()
        {
            if (handle == IntPtr.Zero) return;
            CloseHandle(handle);
            handle = IntPtr.Zero;
        }

        internal sealed class Child : IDisposable
        {
            private IntPtr handle;

            public Child(IntPtr process) { handle = process; }

            public bool Running { get { return WaitForSingleObject(handle, 0) == WaitTimeout; } }

            public uint ExitCode
            {
                get { uint code; if (!GetExitCodeProcess(handle, out code)) throw new Win32Exception(); return code; }
            }

            public void Dispose()
            {
                if (handle == IntPtr.Zero) return;
                CloseHandle(handle);
                handle = IntPtr.Zero;
            }
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct BasicLimits
        {
            public long ProcessTime, JobTime;
            public uint LimitFlags;
            public UIntPtr MinimumWorkingSet, MaximumWorkingSet;
            public uint ActiveProcessLimit;
            public UIntPtr Affinity;
            public uint PriorityClass, SchedulingClass;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ExtendedLimits
        {
            public BasicLimits Basic;
            public ulong ReadOperations, WriteOperations, OtherOperations, ReadBytes, WriteBytes, OtherBytes;
            public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct SecurityAttributes { public int Size; public IntPtr Descriptor; public int Inherit; }

        [StructLayout(LayoutKind.Sequential)]
        private struct StartupInfo
        {
            public int Size;
            public IntPtr Reserved, Desktop, Title;
            public int X, Y, Width, Height, XChars, YChars, Fill, Flags;
            public ushort Show, ReservedSize;
            public IntPtr ReservedBytes, Input, Output, Error;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ProcessInfo { public IntPtr Process, Thread; public uint Pid, Tid; }

        [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode, ExactSpelling = true)]
        private static extern IntPtr CreateJobObjectW(IntPtr attributes, string name);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool SetInformationJobObject(IntPtr job, int kind, ref ExtendedLimits info, uint length);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
        [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode, ExactSpelling = true)]
        private static extern IntPtr CreateFileW(string name, uint access, uint share, ref SecurityAttributes security, uint creation, uint flags, IntPtr template);
        [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode, ExactSpelling = true)]
        private static extern bool CreateProcessW(string application, StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes,
            bool inherit, uint creationFlags, IntPtr environment, string directory, ref StartupInfo startup, out ProcessInfo process);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint ResumeThread(IntPtr thread);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint WaitForSingleObject(IntPtr handle, uint timeout);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetExitCodeProcess(IntPtr process, out uint code);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool TerminateProcess(IntPtr process, uint code);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);
    }
}
