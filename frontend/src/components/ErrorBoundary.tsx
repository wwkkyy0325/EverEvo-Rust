import { Component, type ReactNode } from 'react';

interface Props { children: ReactNode; }
interface State { error: Error | null; }

export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error) { return { error }; }

  render() {
    if (this.state.error) {
      return (
        <div className="flex items-center justify-center h-32">
          <div className="text-center">
            <p className="text-sm text-destructive mb-1">渲染错误</p>
            <p className="text-xs text-muted-foreground">{this.state.error.message}</p>
            <button onClick={() => this.setState({ error: null })}
              className="mt-2 text-xs text-primary hover:underline">重试</button>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}
