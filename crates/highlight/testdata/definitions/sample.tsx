import { useState } from "react";

export interface Props {
  name: string;
}

export function Greeting({ name }: Props) {
  const [count, setCount] = useState(0);
  const onClick = () => setCount(count + 1);
  return (
    <button onClick={onClick}>
      Hello, {name} ({count})
    </button>
  );
}

export const Badge = ({ name }: Props) => <span>{name}</span>;

export class Panel extends React.Component<Props> {
  render() {
    return <div>{this.props.name}</div>;
  }
}
