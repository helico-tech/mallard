import { Component, type ReactNode } from "react";
import { ErrorState, Page } from "@/components/ui-bits";

/** One broken screen never blanks the app; the side nav stays, moving on resets it. */
export class ErrorBoundary extends Component<{ children: ReactNode; resetKey: string }, { error: unknown }> {
  override state: { error: unknown } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error };
  }

  override componentDidUpdate(prev: { resetKey: string }) {
    if (prev.resetKey !== this.props.resetKey && this.state.error) this.setState({ error: null });
  }

  override render() {
    if (!this.state.error) return this.props.children;
    return (
      <Page>
        <ErrorState error={this.state.error} title="This screen could not be shown" onRetry={() => this.setState({ error: null })} />
      </Page>
    );
  }
}
