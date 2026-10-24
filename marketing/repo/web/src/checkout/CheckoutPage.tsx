import { useState } from "react";
import { AddressStep } from "./AddressStep";
import { CardStep } from "./CardStep";
import { ReviewStep } from "./ReviewStep";
import { messages } from "./messages";

type Step = "address" | "card" | "review";

export function CheckoutPage({ invoice }: { invoice: Invoice }) {
  const [step, setStep] = useState<Step>("address");
  const [error, setError] = useState<string | null>(null);

  return (
    <main className="checkout">
      <h1>Pay invoice {invoice.number}</h1>
      {error && <p className="error">{error}</p>}
      {step === "address" && <AddressStep onNext={() => setStep("card")} />}
      {step === "card" && (
        <CardStep
          onNext={() => setStep("review")}
          onError={(code) => setError(messages[code] ?? messages.unknown)}
        />
      )}
      {step === "review" && <ReviewStep invoice={invoice} />}
    </main>
  );
}
