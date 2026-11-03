import { DraftProvider, LiveProvider } from './api/live';
import { DevBar } from './dev/DevBar';
import { useRoute } from './lib/router';
import { FullscreenPage } from './pages/Fullscreen';
import { OpsPage } from './pages/Ops';
import { DevicePage, LogsPage, NetworkPage, RecordingPage } from './pages/Placeholders';
import { TransportSettingsPage } from './pages/TransportSettings';
import { VideoSettingsPage } from './pages/VideoSettings';

function Screen() {
  const route = useRoute();
  switch (route) {
    case 'ops':
      return <OpsPage />;
    case 'video':
      return <VideoSettingsPage />;
    case 'transport':
      return <TransportSettingsPage />;
    case 'recording':
      return <RecordingPage />;
    case 'network':
      return <NetworkPage />;
    case 'logs':
      return <LogsPage />;
    case 'device':
      return <DevicePage />;
    case 'fullscreen':
      return <FullscreenPage />;
  }
}

export function App() {
  return (
    <LiveProvider>
      <DraftProvider>
        <Screen />
        {import.meta.env.DEV && <DevBar />}
      </DraftProvider>
    </LiveProvider>
  );
}
