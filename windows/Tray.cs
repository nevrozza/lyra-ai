using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Linq;
using System.Net.Http;
using System.Net.NetworkInformation;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;
using System.Web.Script.Serialization;
using System.Windows.Forms;

namespace LyraTray
{
    public sealed class Session : ApplicationContext
    {
        private const string OllamaUrl = "http://127.0.0.1:11434";
        private const int OllamaPort = 11434;
        private const string AgentBinary = "lyra-agent";
        private static readonly Regex ValidModel = new Regex(@"\A[A-Za-z0-9][A-Za-z0-9._:/-]*\z");

        private readonly string root, ollamaExecutable, requestedModel, logs, piDirectory, dashboard;
        private readonly int port;
        private readonly string startupDirectory, target;
        private readonly ProcessJob job;
        private readonly System.Windows.Forms.Timer timer;
        private readonly HttpClient http;
        private readonly JavaScriptSerializer json = new JavaScriptSerializer();
        private readonly List<StreamReader> startupLogs = new List<StreamReader>();
        private readonly char[] logBuffer = new char[16384];
        private NotifyIcon icon;
        private Icon trayImage;
        private ContextMenuStrip menu;
        private Exception startupError;
        private ProcessJob.Child build, ollama, lyra;
        private Task<string> probe;
        private Stage stage = Stage.Building;
        private long deadline;
        private bool stopped;
        private bool quitting;
        private bool openingDashboard;
        private bool dashboardOpened;
        private long lastDashboardLaunch;
        private string waitingFor;

        private enum Stage { Building, WaitingForPorts, StartingOllama, StartingLyra, Ready }

        public static void Run(string root, string cargo, string ollama, string model, int port, string startupDirectory, string target)
        {
            if (port == OllamaPort) throw new ArgumentException("The dashboard and Ollama need different ports.");
            using (var singleInstance = new Mutex(false, @"Local\LyraAI.Tray"))
            {
                bool owned;
                try { owned = singleInstance.WaitOne(0); }
                catch (AbandonedMutexException) { owned = true; }
                if (!owned)
                {
                    throw new InvalidOperationException("Lyra is already starting or running.");
                }
                try
                {
                    Application.EnableVisualStyles();
                    using (var session = new Session(root, cargo, ollama, model, port, startupDirectory, target))
                    {
                        Application.Run(session);
                        if (session.startupError != null) throw new InvalidOperationException(session.startupError.Message, session.startupError);
                    }
                }
                finally { singleInstance.ReleaseMutex(); }
            }
        }

        private Session(string root, string cargo, string ollama, string model, int port, string startupDirectory, string target)
        {
            this.root = root;
            this.port = port;
            this.startupDirectory = startupDirectory;
            this.target = target;
            ollamaExecutable = ollama;
            requestedModel = model;
            dashboard = "http://127.0.0.1:" + port + "/";
            var local = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "LyraAI");
            logs = Path.Combine(local, "logs");
            piDirectory = Path.Combine(local, "pi-agent");
            Directory.CreateDirectory(logs);

            job = new ProcessJob();
            http = new HttpClient(new HttpClientHandler { UseProxy = false, AllowAutoRedirect = false }) { Timeout = TimeSpan.FromSeconds(1) };
            timer = new System.Windows.Forms.Timer { Interval = 350 };
            timer.Tick += OnTick;

            try
            {
                Console.WriteLine("Building Lyra in release mode...");
                build = job.Start(cargo, "build --release --locked --bin " + AgentBinary +
                    " --target-dir \"" + target + "\"", root, Log("build"));
                FollowLog("build");
                timer.Start();
            }
            catch
            {
                Dispose();
                throw;
            }
        }

        private void ShowTray()
        {
            menu = new ContextMenuStrip();
            menu.Items.Add("Quit", null, async delegate { await QuitAsync(); });
            trayImage = new Icon(Path.Combine(root, "assets", "lyra.ico"));
            icon = new NotifyIcon { Icon = trayImage, Text = "Lyra AI: open dashboard", ContextMenuStrip = menu };
            icon.MouseClick += delegate(object sender, MouseEventArgs click)
            {
                if (click.Button == MouseButtons.Left) OpenDashboard();
            };
            icon.Visible = true;
        }

        private void OnTick(object sender, EventArgs args)
        {
            try
            {
                PrintLogs();
                if (stage == Stage.Building)
                {
                    if (build.Running) return;
                    uint exitCode = build.ExitCode;
                    build.Dispose();
                    build = null;
                    if (exitCode != 0) throw new InvalidOperationException("Cargo build failed (exit code " + exitCode + "). See " + Log("build"));
                    Console.WriteLine("Build finished.");
                    stage = Stage.WaitingForPorts;
                }

                if (stage == Stage.WaitingForPorts)
                {
                    var listeners = IPGlobalProperties.GetIPGlobalProperties().GetActiveTcpListeners();
                    string blocked = listeners.Any(endpoint => endpoint.Port == OllamaPort)
                        ? "Quit the existing Ollama from its own tray icon. Lyra will then start an Ollama it can shut down."
                        : listeners.Any(endpoint => endpoint.Port == port)
                            ? "Dashboard port " + port + " is in use. Stop the other server or relaunch with -Port."
                            : null;
                    if (blocked != null)
                    {
                        if (waitingFor != blocked) Console.WriteLine(blocked);
                        waitingFor = blocked;
                        timer.Interval = 1000;
                        return;
                    }
                    waitingFor = null;
                    Console.WriteLine("Starting Ollama at " + OllamaUrl + "...");
                    ollama = StartWithEnvironment(ollamaExecutable, "serve", "ollama", "OLLAMA_HOST", "127.0.0.1:11434");
                    FollowLog("ollama");
                    BeginProbe(Stage.StartingOllama);
                }

                CheckAlive(ollama, "Ollama", Log("ollama"));
                if (stage == Stage.StartingOllama)
                {
                    if (ReadWhenReady(OllamaUrl + "/api/tags") == null) return;
                    string model = PreferredModel();
                    Console.WriteLine("Ollama is ready. Starting Lyra...");
                    string executable = Path.Combine(target, "release", AgentBinary + ".exe");
                    string modelArgument = string.IsNullOrEmpty(model) ? "" : " --model \"" + model + "\"";
                    lyra = job.Start(executable, "--port " + port + modelArgument +
                        " --pi-agent-dir \"" + piDirectory + "\"" +
                        " --startup-log \"" + Path.Combine(startupDirectory, "startup.log") + "\"",
                        root, Log("lyra"));
                    FollowLog("lyra");
                    BeginProbe(Stage.StartingLyra);
                }

                CheckAlive(lyra, "Lyra", Log("lyra"));
                if (stage == Stage.StartingLyra)
                {
                    if (ReadWhenReady(dashboard) == null) return;
                    PrintLogs(drain: true);
                    StopFollowingLogs();
                    ShowTray();
                    Console.WriteLine("Ready: " + dashboard);
                    File.WriteAllText(Path.Combine(startupDirectory, "ready"), "");
                    stage = Stage.Ready;
                    timer.Interval = 2000;
                    OpenDashboard(installedOnly: true);
                }
            }
            catch (Exception error)
            {
                if (stage != Stage.Ready) startupError = error;
                ExitThread();
                if (startupError == null) MessageBox.Show(error.Message, "Lyra AI", MessageBoxButtons.OK, MessageBoxIcon.Error);
            }
        }

        private string PreferredModel()
        {
            string desired = requestedModel;
            var original = Environment.GetEnvironmentVariable("PI_CODING_AGENT_DIR");
            if (string.IsNullOrWhiteSpace(original)) original = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".pi", "agent");
            if (string.IsNullOrWhiteSpace(desired))
            {
                var settings = Path.Combine(original, "settings.json");
                if (File.Exists(settings))
                {
                    try
                    {
                        var config = json.Deserialize<PiSettings>(File.ReadAllText(settings));
                        if (config != null) desired = config.defaultModel;
                    }
                    catch (ArgumentException) { /* Pi may have a config this launcher cannot read. */ }
                    catch (InvalidOperationException) { /* Use an installed model below. */ }
                }
            }
            return !string.IsNullOrEmpty(desired) && ValidModel.IsMatch(desired) ? desired : null;
        }

        private ProcessJob.Child StartWithEnvironment(string executable, string arguments, string log, string name, string value)
        {
            string original = Environment.GetEnvironmentVariable(name);
            Environment.SetEnvironmentVariable(name, value);
            try { return job.Start(executable, arguments, root, Log(log)); }
            finally { Environment.SetEnvironmentVariable(name, original); }
        }

        private string Log(string name) { return Path.Combine(logs, name + ".log"); }

        private void FollowLog(string name)
        {
            var file = new FileStream(Log(name), FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
            startupLogs.Add(new StreamReader(file, Encoding.UTF8));
        }

        private void PrintLogs(bool drain = false)
        {
            foreach (var log in startupLogs)
            {
                int count;
                do
                {
                    count = log.Read(logBuffer, 0, logBuffer.Length);
                    if (count > 0) Console.Write(logBuffer, 0, count);
                } while (drain && count == logBuffer.Length);
            }
        }

        private void StopFollowingLogs()
        {
            foreach (var log in startupLogs) log.Dispose();
            startupLogs.Clear();
        }

        private static void CheckAlive(ProcessJob.Child child, string name, string log)
        {
            if (!child.Running) throw new InvalidOperationException(name + " exited (code " + child.ExitCode + "). See " + log);
        }

        private void BeginProbe(Stage next)
        {
            stage = next;
            probe = null;
            deadline = Stopwatch.GetTimestamp() + 60 * Stopwatch.Frequency;
            timer.Interval = 350;
        }

        private string ReadWhenReady(string url)
        {
            if (Stopwatch.GetTimestamp() >= deadline)
                throw new TimeoutException("Startup timed out at " + url + ". Logs: " + logs);
            if (probe == null)
            {
                probe = http.GetStringAsync(url);
                return null;
            }
            if (!probe.IsCompleted) return null;
            var completed = probe;
            probe = null;
            return completed.Status == TaskStatus.RanToCompletion ? completed.Result : ObserveFailure(completed);
        }

        private static string ObserveFailure(Task task)
        {
            if (task.Exception != null) task.Exception.Handle(error => true);
            return null;
        }

        private async void OpenDashboard(bool installedOnly = false)
        {
            if (openingDashboard || quitting) return;
            if (lastDashboardLaunch != 0 && Stopwatch.GetTimestamp() - lastDashboardLaunch < 2 * Stopwatch.Frequency) return;
            openingDashboard = true;
            try
            {
                string appName = InstalledDashboard.AppName(dashboard);
                IntPtr window = await Task.Run(() => InstalledDashboard.FindWindow(appName));
                if (stopped || quitting) return;
                bool installedDashboard = window != IntPtr.Zero;
                if (installedOnly && installedDashboard) return;
                if (!installedDashboard)
                {
                    bool launched = await Task.Run(() => InstalledDashboard.TryLaunch(appName));
                    if (stopped || quitting) return;
                    if (launched)
                    {
                        dashboardOpened = true;
                        lastDashboardLaunch = Stopwatch.GetTimestamp();
                        return;
                    }
                    if (installedOnly) return;
                    window = await Task.Run(() => BrowserTab.SelectExisting());
                }
                if (stopped || quitting) return;
                if (window != IntPtr.Zero)
                {
                    BrowserTab.Activate(window);
                    if (!dashboardOpened)
                    {
                        await Task.Delay(120);
                        if (!stopped && !quitting) dashboardOpened = BrowserTab.RefreshOnFirstOpen(window, installedDashboard);
                    }
                    return;
                }
                using (Process.Start(new ProcessStartInfo(dashboard) { UseShellExecute = true })) { }
                dashboardOpened = true;
                lastDashboardLaunch = Stopwatch.GetTimestamp();
            }
            catch (Exception error)
            {
                if (!stopped) icon.ShowBalloonTip(5000, "Lyra AI", error.Message, ToolTipIcon.Error);
            }
            finally { openingDashboard = false; }
        }

        private async Task QuitAsync()
        {
            if (quitting || stopped) return;
            quitting = true;
            timer.Stop();
            try
            {
                using (var request = new HttpRequestMessage(HttpMethod.Post, dashboard + "shutdown"))
                {
                    request.Headers.TryAddWithoutValidation("Origin", dashboard.TrimEnd('/'));
                    using (var response = await http.SendAsync(request))
                    {
                        // Keep the dashboard alive long enough to deliver its final SSE update.
                        if (response.IsSuccessStatusCode) await Task.Delay(500);
                    }
                }
            }
            catch (HttpRequestException) { }
            catch (TaskCanceledException) { }
            finally { ExitThread(); }
        }

        protected override void ExitThreadCore()
        {
            Stop();
            base.ExitThreadCore();
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing) Stop();
            base.Dispose(disposing);
        }

        private void Stop()
        {
            if (stopped) return;
            stopped = true;
            timer.Stop();
            StopFollowingLogs();
            job.Dispose();
            if (build != null) build.Dispose();
            if (lyra != null) lyra.Dispose();
            if (ollama != null) ollama.Dispose();
            http.Dispose();
            if (icon != null) { icon.Visible = false; icon.Dispose(); }
            if (trayImage != null) trayImage.Dispose();
            if (menu != null) menu.Dispose();
            timer.Dispose();
        }

        public sealed class PiSettings { public string defaultModel { get; set; } }
    }
}
