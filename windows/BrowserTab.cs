using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Windows.Automation;
using System.Windows.Forms;

namespace LyraTray
{
    internal static class BrowserTab
    {
        private const string DashboardTitle = "Lyra";
        private const string LegacyDashboardTitle = "Lyra AI";
        private const string LogSuffix = " \u2014 \u0416\u0443\u0440\u043d\u0430\u043b \u0437\u0430\u043f\u0443\u0441\u043a\u0430";
        private const string LogTitle = DashboardTitle + LogSuffix;
        private const string LegacyLogTitle = LegacyDashboardTitle + LogSuffix;

        public static IntPtr SelectExisting()
        {
            var browserClasses = new OrCondition(
                new PropertyCondition(AutomationElement.ClassNameProperty, "Chrome_WidgetWin_1"),
                new PropertyCondition(AutomationElement.ClassNameProperty, "MozillaWindowClass"));
            var lyraTabs = new AndCondition(
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.TabItem),
                new OrCondition(
                    new PropertyCondition(AutomationElement.NameProperty, DashboardTitle),
                    new PropertyCondition(AutomationElement.NameProperty, LogTitle),
                    new PropertyCondition(AutomationElement.NameProperty, LegacyDashboardTitle),
                    new PropertyCondition(AutomationElement.NameProperty, LegacyLogTitle)));

            try
            {
                foreach (AutomationElement browser in AutomationElement.RootElement.FindAll(TreeScope.Children, browserClasses))
                {
                    IntPtr window = SelectInWindow(browser, lyraTabs);
                    if (window != IntPtr.Zero) return window;
                }
            }
            catch (COMException) { }
            catch (ElementNotAvailableException) { }
            return IntPtr.Zero;
        }

        private static IntPtr SelectInWindow(AutomationElement browser, Condition lyraTabs)
        {
            try
            {
                var window = new IntPtr(browser.Current.NativeWindowHandle);
                if (window == IntPtr.Zero) return window;
                foreach (AutomationElement tab in browser.FindAll(TreeScope.Descendants, lyraTabs))
                {
                    object pattern;
                    if (tab.TryGetCurrentPattern(SelectionItemPattern.Pattern, out pattern))
                    {
                        ((SelectionItemPattern)pattern).Select();
                        return window;
                    }
                    if (tab.TryGetCurrentPattern(InvokePattern.Pattern, out pattern))
                    {
                        ((InvokePattern)pattern).Invoke();
                        return window;
                    }
                }
                // A browser without an accessible tab strip may still have Lyra selected.
                string title = browser.Current.Name;
                if (HasTitle(title, DashboardTitle) || HasTitle(title, LogTitle) ||
                    HasTitle(title, LegacyDashboardTitle) || HasTitle(title, LegacyLogTitle)) return window;
            }
            catch (COMException) { }
            catch (ElementNotAvailableException) { }
            catch (InvalidOperationException) { }
            return IntPtr.Zero;
        }

        public static void Activate(IntPtr window)
        {
            if (IsIconic(window)) ShowWindowAsync(window, 9); // SW_RESTORE
            if (!SetForegroundWindow(window)) FlashWindow(window, true);
        }

        public static bool RefreshOnFirstOpen(IntPtr window)
        {
            if (GetForegroundWindow() != window) return false;
            var windowTitle = new StringBuilder(512);
            if (GetWindowText(window, windowTitle, windowTitle.Capacity) == 0) return false;
            string title = windowTitle.ToString();
            // Current dashboards reload themselves when the server reports new assets.
            // Older dashboards and the static log page still need a native reload.
            if (HasTitle(title, LegacyDashboardTitle) || HasTitle(title, LegacyLogTitle) || HasTitle(title, LogTitle))
            {
                SendKeys.SendWait("{F5}");
                return true;
            }
            return HasTitle(title, DashboardTitle);
        }

        private static bool HasTitle(string windowTitle, string tabTitle)
        {
            return windowTitle == tabTitle ||
                windowTitle.StartsWith(tabTitle + " - ", StringComparison.Ordinal) ||
                windowTitle.StartsWith(tabTitle + " \u2014 ", StringComparison.Ordinal);
        }

        [DllImport("user32.dll")]
        private static extern IntPtr GetForegroundWindow();

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int GetWindowText(IntPtr window, StringBuilder title, int capacity);

        [DllImport("user32.dll")]
        private static extern bool IsIconic(IntPtr window);

        [DllImport("user32.dll")]
        private static extern bool ShowWindowAsync(IntPtr window, int command);

        [DllImport("user32.dll")]
        private static extern bool SetForegroundWindow(IntPtr window);

        [DllImport("user32.dll")]
        private static extern bool FlashWindow(IntPtr window, bool invert);
    }
}
