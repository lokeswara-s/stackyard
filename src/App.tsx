import { lazy } from "react";
import { Navigate, Route, Routes } from "react-router-dom";

import AppShell from "./components/layout/AppShell";
import DashboardPage from "./pages/DashboardPage";

const ContainersPage = lazy(() => import("./pages/ContainersPage"));
const DependencyGraphPage = lazy(() => import("./pages/DependencyGraphPage"));
const EnvPortsPage = lazy(() => import("./pages/EnvPortsPage"));
const PrerequisitesPage = lazy(() => import("./pages/PrerequisitesPage"));
const FaultInjectionPage = lazy(() => import("./pages/FaultInjectionPage"));
const GitMultiRepoPage = lazy(() => import("./pages/GitMultiRepoPage"));
const LogsPage = lazy(() => import("./pages/LogsPage"));
const PipelinesPage = lazy(() => import("./pages/PipelinesPage"));
const PluginMarketplacePage = lazy(() => import("./pages/PluginMarketplacePage"));
const PortsPage = lazy(() => import("./pages/PortsPage"));
const ServiceDetailPage = lazy(() => import("./pages/ServiceDetailPage"));
const SettingsPage = lazy(() => import("./pages/SettingsPage"));
const UpPipelinePage = lazy(() => import("./pages/UpPipelinePage"));
const WorkspaceEditorPage = lazy(() => import("./pages/WorkspaceEditorPage"));
const WorkspacesPage = lazy(() => import("./pages/WorkspacesPage"));

export default function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route index element={<DashboardPage />} />
        <Route path="up" element={<UpPipelinePage />} />
        <Route path="services/:name" element={<ServiceDetailPage />} />
        <Route path="dependency-graph" element={<DependencyGraphPage />} />
        <Route path="workspaces" element={<WorkspacesPage />} />
        <Route path="workspaces/new" element={<WorkspaceEditorPage />} />
        <Route path="workspaces/:id/edit" element={<WorkspaceEditorPage />} />
        <Route path="containers-volumes" element={<ContainersPage />} />
        <Route path="ports-proxies" element={<PortsPage />} />
        <Route path="git" element={<GitMultiRepoPage />} />
        <Route path="logs" element={<LogsPage />} />
        <Route path="environment" element={<EnvPortsPage />} />
        <Route path="prerequisites" element={<PrerequisitesPage />} />
        <Route path="pipelines" element={<PipelinesPage />} />
        <Route path="plugins" element={<PluginMarketplacePage />} />
        <Route path="fault-injection" element={<FaultInjectionPage />} />
        <Route path="settings" element={<SettingsPage />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
