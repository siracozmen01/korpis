// SPDX-License-Identifier: AGPL-3.0-or-later

// A change proposed and then confirmed, interrupting as much as its plan's risk calls for (§4 of
// docs/11-surface-web.md, tier.ts): applied at once and shown after, previewed in place with one
// confirmation, or reviewed on a screen of its own with an explicit confirmation. A plan that
// needs someone else's authority (§3.6 of docs/08-identity.md) is left waiting for them, and the
// screen says so. Whatever proposes it, a power change or a new configuration, is confirmed by
// the plan's identifier, so the plan reviewed is the plan that runs.

import { useState, type ReactNode } from "react";
import { Button, CheckboxButton, CheckboxField } from "react-aria-components";
import { useReach } from "../clients";
import { PlanApproval, PlanStatus, type Plan } from "../gen/korpis/v1/workload_pb";
import { t } from "../i18n/intl";
import { refusal } from "../views/refusal";
import { confirm, withdraw } from "./act";
import { PlanView } from "./PlanView";
import { tierOf } from "./tier";

type Step =
  | { readonly kind: "idle" }
  | { readonly kind: "busy" }
  | { readonly kind: "preview"; readonly label: string; readonly plan: Plan }
  | { readonly kind: "review"; readonly label: string; readonly plan: Plan }
  | { readonly kind: "waiting"; readonly plan: Plan }
  | { readonly kind: "done"; readonly label: string; readonly plan: Plan };

/** A change in progress, and what the screen shows of it. */
export interface Planned {
  /** Whether a change may be begun now. */
  readonly ready: boolean;
  /** Proposes a change named `label`, whose plan `propose` answers, and carries it on. */
  readonly begin: (label: string, propose: () => Promise<Plan | undefined>) => void;
  /** The preview, the wait, or what was done, shown in place. */
  readonly shown: ReactNode;
  /** The whole plan on a screen of its own, which takes the place of whatever began it. */
  readonly review: ReactNode;
}

/** The flow of a change from its proposal to its plan applied. */
export function usePlanned(): Planned {
  const { clients } = useReach();
  const [step, setStep] = useState<Step>({ kind: "idle" });
  const [error, setError] = useState<string>();
  const run = (work: () => Promise<Step>) => {
    setError(undefined);
    setStep({ kind: "busy" });
    work().then(setStep, (e: unknown) => {
      setError(refusal(e));
      setStep({ kind: "idle" });
    });
  };
  const confirmed = (label: string, plan: Plan) => async (): Promise<Step> => ({
    kind: "done",
    label,
    plan: (await confirm(clients, plan)) ?? plan,
  });
  const cancel = (plan: Plan) => {
    run(async () => {
      await withdraw(clients, plan);
      return { kind: "idle" };
    });
  };
  const begin = (label: string, propose: () => Promise<Plan | undefined>) => {
    run(async () => {
      const plan = await propose();
      if (plan === undefined) {
        throw new Error(t({ defaultMessage: "the control plane sent no plan", id: "TAhO4thL" }));
      }
      const waits = plan.status !== PlanStatus.PROPOSED || plan.approval === PlanApproval.AUTHORITY;
      if (waits) return { kind: "waiting", plan };
      const tier = tierOf(plan);
      if (tier === "direct") return confirmed(label, plan)();
      return { kind: tier === "confirm" ? "preview" : "review", label, plan };
    });
  };
  const alert = error !== undefined && <p role="alert">{error}</p>;
  const review = step.kind === "review" && (
    <Review
      label={step.label}
      plan={step.plan}
      alert={alert}
      confirm={() => {
        run(confirmed(step.label, step.plan));
      }}
      cancel={() => {
        cancel(step.plan);
      }}
    />
  );
  const shown = (
    <>
      {step.kind === "preview" && (
        <div
          aria-label={t({
            defaultMessage: "Preview",
            id: "G3xFadAQ",
            description: "a plan previewed",
          })}
        >
          <p>
            {t(
              { defaultMessage: "{action} will do this:", id: "tiVJHECN" },
              { action: step.label },
            )}
          </p>
          <PlanView plan={step.plan} />
          <Button
            onPress={() => {
              run(confirmed(step.label, step.plan));
            }}
          >
            {t({ defaultMessage: "Confirm", id: "N2IrpMDH" })}
          </Button>
          <Button
            onPress={() => {
              cancel(step.plan);
            }}
          >
            {t({ defaultMessage: "Cancel", id: "47FYwba+" })}
          </Button>
        </div>
      )}
      {step.kind === "waiting" && (
        <p role="status">
          {t(
            { defaultMessage: "Its plan waits for approval: {why}", id: "AZ8OrWtY" },
            {
              why:
                step.plan.impact?.approvalReason ??
                t({ defaultMessage: "someone must approve it", id: "zb3eS1/R" }),
            },
          )}
        </p>
      )}
      {step.kind === "done" && (
        <details>
          <summary>
            {t(
              { defaultMessage: "{action}: done. What it did", id: "0bWtzCgd" },
              { action: step.label },
            )}
          </summary>
          <PlanView plan={step.plan} />
        </details>
      )}
      {alert}
    </>
  );
  return { ready: step.kind === "idle" || step.kind === "done", begin, shown, review };
}

/** The whole plan, on its own, with an explicit confirmation. */
function Review(props: {
  readonly label: string;
  readonly plan: Plan;
  readonly alert: ReactNode;
  readonly confirm: () => void;
  readonly cancel: () => void;
}) {
  const [read, setRead] = useState(false);
  return (
    <section aria-labelledby="review">
      <h2 id="review">
        {t({ defaultMessage: "Review: {action}", id: "aMLvvtkX" }, { action: props.label })}
      </h2>
      <p>
        {t({ defaultMessage: "Every step it takes, and every object it acts on:", id: "e258PFM2" })}
      </p>
      <PlanView plan={props.plan} />
      <CheckboxField isSelected={read} onChange={setRead}>
        <CheckboxButton>
          {t({
            defaultMessage:
              "I have read every step, and what is marked irreversible cannot be undone",
            id: "Jby50rYV",
          })}
        </CheckboxButton>
      </CheckboxField>
      <Button isDisabled={!read} onPress={props.confirm}>
        {props.label}
      </Button>
      <Button onPress={props.cancel}>{t({ defaultMessage: "Cancel", id: "47FYwba+" })}</Button>
      {props.alert}
    </section>
  );
}
