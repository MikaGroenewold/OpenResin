import { Component, type ReactNode } from "react";
import { t as tr } from "./i18n";

interface State {
  error: Error | null;
}

/** Shows a recoverable error screen instead of a blank window. */
export class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error) {
    console.error("UI error", error);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="crash">
        <h2>{tr("Something went wrong in the interface.")}</h2>
        <p className="muted">{tr("Your plate is kept by the application; reloading the interface brings it back.")}</p>
        <pre>{String(this.state.error?.message ?? this.state.error)}</pre>
        <button className="btn primary" onClick={() => window.location.reload()}>
          {tr("Reload interface")}
        </button>
      </div>
    );
  }
}
