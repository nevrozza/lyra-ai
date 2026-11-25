using System;
using System.Runtime.InteropServices;
using System.Windows.Automation;

namespace LyraTray
{
    internal static class BrowserTab
    {
        private const string DashboardTitle = "Lyra AI";
        private const string LogTitle = "Lyra AI \u2014 \u0416\u0443\u0440\u043d\u0430\u043b \u0437\u0430\u043f\u0443\u0441\u043a\u0430";

        public static IntPtr SelectExisting()
        {
            var browserClasses = new OrCondition(
                new PropertyCondition(AutomationElement.ClassNameProperty, "Chrome_WidgetWin_1"),
                new PropertyCondition(AutomationElement.ClassNameProperty, "MozillaWindowClass"));
            var lyraTabs = new AndCondition(
                new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.TabItem),
                new OrCondition(
                    new PropertyCondition(AutomationElement.NameProperty, DashboardTitle),
                    new PropertyCondition(AutomationElement.NameProperty, LogTitle)));

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
                if (title == DashboardTitle || title == LogTitle ||
                    title.StartsWith(DashboardTitle + " - ", StringComparison.Ordinal) ||
                    title.StartsWith(LogTitle + " - ", StringComparison.Ordinal)) return window;
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
