// React hooks shared by react-dom (desktop) and react-native (phone): no DOM APIs here.
import { useEffect, useState } from "react";

/** Latest value of a subscription op, e.g. useSubscription(api.job, { job }). */
export function useSubscription<I, O>(op: (input: I, onValue: (v: O) => void) => () => void, input: I): O | undefined {
  const [value, setValue] = useState<O>();
  const key = JSON.stringify(input);
  useEffect(() => op(input, setValue), [op, key]);
  return value;
}
