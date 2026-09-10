import { DashboardPage } from "./pages/DashboardPage";
import { ThemeProvider } from "./lib/theme";
import { IdPrivacyProvider } from "./lib/idPrivacy";
import { LocalAnnotationsProvider } from "./lib/localAnnotations";

export default function App() {
  return (
    <ThemeProvider>
      <IdPrivacyProvider>
        <LocalAnnotationsProvider>
          <DashboardPage />
        </LocalAnnotationsProvider>
      </IdPrivacyProvider>
    </ThemeProvider>
  );
}
