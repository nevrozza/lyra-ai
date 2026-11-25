using System;
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
        private static readonly Regex ValidModel = new Regex(@"\A[A-Za-z0-9][A-Za-z0-9._:/-]*\z");

        private readonly string root, ollamaExecutable, requestedModel, logs, piDirectory, dashboard;
        private readonly int port;
        private readonly ProcessJob job;
        private readonly NotifyIcon icon;
        private readonly ContextMenuStrip menu;
        private readonly System.Windows.Forms.Timer timer;
        private readonly HttpClient http;
        private readonly JavaScriptSerializer json = new JavaScriptSerializer();
        private ProcessJob.Child build, ollama, lyra;
        private Task<string> probe;
        private Stage stage = Stage.Building;
        private long deadline;
        private bool openWhenReady, stopped;
        private string waitingFor;

        private enum Stage { Building, WaitingForPorts, StartingOllama, StartingLyra, Ready }

        public static void Run(string root, string cargo, string ollama, string model, int port)
        {
            if (port == OllamaPort) throw new ArgumentException("The dashboard and Ollama need different ports.");
            using (var singleInstance = new Mutex(false, @"Local\LyraAI.Tray"))
            {
                bool owned;
                try { owned = singleInstance.WaitOne(0); }
                catch (AbandonedMutexException) { owned = true; }
                if (!owned)
                {
                    MessageBox.Show("Lyra is already running. Use its tray icon.", "Lyra AI");
                    return;
                }
                try
                {
                    Application.EnableVisualStyles();
                    using (var session = new Session(root, cargo, ollama, model, port)) Application.Run(session);
                }
                finally { singleInstance.ReleaseMutex(); }
            }
        }

        private Session(string root, string cargo, string ollama, string model, int port)
        {
            this.root = root;
            this.port = port;
            ollamaExecutable = ollama;
            requestedModel = model;
            dashboard = "http://127.0.0.1:" + port + "/";
            var local = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "LyraAI");
            logs = Path.Combine(local, "logs");
            piDirectory = Path.Combine(local, "pi-agent");
            Directory.CreateDirectory(logs);

            job = new ProcessJob();
            http = new HttpClient(new HttpClientHandler { UseProxy = false, AllowAutoRedirect = false }) { Timeout = TimeSpan.FromSeconds(1) };
            menu = new ContextMenuStrip();
            menu.Items.Add("Quit", null, delegate { ExitThread(); });
            icon = new NotifyIcon { Icon = SystemIcons.Application, Text = "Lyra AI: building", ContextMenuStrip = menu, Visible = true };
            icon.MouseClick += OnTrayClick;
            timer = new System.Windows.Forms.Timer { Interval = 350 };
            timer.Tick += OnTick;

            try
            {
                var target = Path.Combine(root, "target");
                build = job.Start(cargo, "build --release --locked --bin local-agent --target-dir \"" + target + "\"", root, Log("build"));
                timer.Start();
            }
            catch
            {
                Dispose();
                throw;
            }
        }

        private void OnTrayClick(object sender, MouseEventArgs e)
        {
            if (e.Button != MouseButtons.Left) return;
            if (stage == Stage.Ready)
            {
                OpenDashboard();
                return;
            }
            openWhenReady = true;
            icon.ShowBalloonTip(3500, "Lyra AI", waitingFor ?? "The dashboard will open when Lyra is ready.", ToolTipIcon.Info);
        }

        private void OnTick(object sender, EventArgs args)
        {
            try
            {
                if (stage == Stage.Building)
                {
                    if (build.Running) return;
                    uint exitCode = build.ExitCode;
                    build.Dispose();
                    build = null;
                    if (exitCode != 0) throw new InvalidOperationException("Cargo build failed (exit code " + exitCode + "). See " + Log("build"));
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
                        if (waitingFor != blocked) icon.ShowBalloonTip(8000, "Lyra AI", blocked, ToolTipIcon.Info);
                        waitingFor = blocked;
                        icon.Text = "Lyra AI: waiting for a port";
                        timer.Interval = 1000;
                        return;
                    }
                    waitingFor = null;
                    ollama = StartWithEnvironment(ollamaExecutable, "serve", "ollama", "OLLAMA_HOST", "127.0.0.1:11434");
                    BeginProbe(Stage.StartingOllama, "Lyra AI: starting Ollama");
                }

                CheckAlive(ollama, "Ollama", Log("ollama"));
                if (stage == Stage.StartingOllama)
                {
                    string tags = ReadWhenReady(OllamaUrl + "/api/tags");
                    if (tags == null) return;
                    string model = ConfigurePi(tags);
                    string executable = Path.Combine(root, "target", "release", "local-agent.exe");
                    lyra = StartWithEnvironment(executable, "--model \"" + model + "\" --port " + port,
                        "lyra", "PI_CODING_AGENT_DIR", piDirectory);
                    BeginProbe(Stage.StartingLyra, "Lyra AI: starting dashboard");
                }

                CheckAlive(lyra, "Lyra", Log("lyra"));
                if (stage == Stage.StartingLyra)
                {
                    if (ReadWhenReady(dashboard) == null) return;
                    stage = Stage.Ready;
                    timer.Interval = 2000;
                    icon.Text = "Lyra AI: open dashboard";
                    if (openWhenReady) OpenDashboard();
                }
            }
            catch (Exception error)
            {
                ExitThread();
                MessageBox.Show(error.Message, "Lyra AI", MessageBoxButtons.OK, MessageBoxIcon.Error);
            }
        }

        private string ConfigurePi(string tags)
        {
            var library = json.Deserialize<OllamaLibrary>(tags);
            var names = (library.models ?? new ModelEntry[0])
                .Where(entry => !string.IsNullOrWhiteSpace(entry.name) && ValidModel.IsMatch(entry.name)
                    && !entry.name.EndsWith(":cloud", StringComparison.OrdinalIgnoreCase)
                    && string.IsNullOrEmpty(entry.remote_host))
                .Select(entry => entry.name).ToArray();
            if (names.Length == 0) throw new InvalidOperationException("No local Ollama model found. Run ollama pull MODEL first.");

            string desired = requestedModel;
            var original = Environment.GetEnvironmentVariable("PI_CODING_AGENT_DIR");
            if (string.IsNullOrWhiteSpace(original)) original = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".pi", "agent");
            if (string.IsNullOrWhiteSpace(desired))
            {
                var settings = Path.Combine(original, "settings.json");
                if (File.Exists(settings))
                {
                    try { desired = json.Deserialize<PiSettings>(File.ReadAllText(settings)).defaultModel; }
                    catch (ArgumentException) { /* Pi may have a config this launcher cannot read. */ }
                    catch (InvalidOperationException) { /* Use an installed model below. */ }
                }
            }
            string model = names.FirstOrDefault(name => string.Equals(name, desired, StringComparison.OrdinalIgnoreCase)
                || string.Equals(name, desired + ":latest", StringComparison.OrdinalIgnoreCase));
            if (model == null && !string.IsNullOrWhiteSpace(requestedModel))
                throw new InvalidOperationException("The requested model is not installed: " + requestedModel);
            model = model ?? names.FirstOrDefault(name => name == "qwen3:8b") ?? names[0];

            Directory.CreateDirectory(piDirectory);
            var configured = new { id = model };
            var config = new { providers = new { ollama = new { baseUrl = OllamaUrl + "/v1", api = "openai-completions", apiKey = "ollama", models = new[] { configured } } } };
            File.WriteAllText(Path.Combine(piDirectory, "models.json"), json.Serialize(config), new UTF8Encoding(false));
            return model;
        }

        private ProcessJob.Child StartWithEnvironment(string executable, string arguments, string log, string name, string value)
        {
            string original = Environment.GetEnvironmentVariable(name);
            Environment.SetEnvironmentVariable(name, value);
            try { return job.Start(executable, arguments, root, Log(log)); }
            finally { Environment.SetEnvironmentVariable(name, original); }
        }

        private string Log(string name) { return Path.Combine(logs, name + ".log"); }

        private static void CheckAlive(ProcessJob.Child child, string name, string log)
        {
            if (!child.Running) throw new InvalidOperationException(name + " exited (code " + child.ExitCode + "). See " + log);
        }

        private void BeginProbe(Stage next, string text)
        {
            stage = next;
            icon.Text = text;
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

        private void OpenDashboard()
        {
            try { Process.Start(new ProcessStartInfo(dashboard) { UseShellExecute = true }); }
            catch (Exception error) { icon.ShowBalloonTip(5000, "Lyra AI", error.Message, ToolTipIcon.Error); }
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
            job.Dispose();
            if (build != null) build.Dispose();
            if (lyra != null) lyra.Dispose();
            if (ollama != null) ollama.Dispose();
            http.Dispose();
            icon.Visible = false;
            icon.Dispose();
            menu.Dispose();
            timer.Dispose();
        }

        public sealed class OllamaLibrary { public ModelEntry[] models { get; set; } }
        public sealed class ModelEntry { public string name { get; set; } public string remote_host { get; set; } }
        public sealed class PiSettings { public string defaultModel { get; set; } }
    }
}
