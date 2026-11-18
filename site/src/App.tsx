import { Footer } from "@/components/Footer";
import { GetStarted } from "@/components/GetStarted";
import { Hero } from "@/components/Hero";
import { Isolation } from "@/components/Isolation";
import { Loop } from "@/components/Loop";
import { Modes } from "@/components/Modes";
import { Permissions } from "@/components/Permissions";
import { Surfaces } from "@/components/Surfaces";
import { Topbar } from "@/components/Topbar";
import { I18nProvider } from "@/lib/i18n";
import { initTheme } from "@/lib/theme";

initTheme();

export default function App() {
  return (
    <I18nProvider>
      <div className="min-h-screen bg-surface-app text-text-primary">
        <Topbar />
        <main>
          <Hero />
          <Loop />
          <Modes />
          <Permissions />
          <Isolation />
          <Surfaces />
          <GetStarted />
        </main>
        <Footer />
      </div>
    </I18nProvider>
  );
}
