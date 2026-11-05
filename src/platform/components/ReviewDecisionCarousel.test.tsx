import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ReviewDecisionCarousel, type ReviewDecisionCarouselOption } from "./ReviewDecisionCarousel";

const options: ReviewDecisionCarouselOption[] = [
  { value: "approve_post", label: "Aprovar", description: "Aprove este merge request no GitLab.", tone: "primary" },
  {
    value: "mark_reviewed",
    label: "Marcar como revisado",
    description: "Feche localmente sem publicar no GitLab.",
    tone: "secondary",
  },
  { value: "skip", label: "Ignorar", description: "Deixe esta revisão sem publicar.", tone: "secondary" },
];

describe("review decision carousel", () => {
  it("renders decisions as accessible buttons instead of a select", () => {
    const html = renderToStaticMarkup(
      <ReviewDecisionCarousel options={options} value="approve_post" onChange={() => undefined} />,
    );
    expect(html).toContain('role="radiogroup"');
    expect(html).toContain('aria-label="Decisões da revisão"');
    expect(html).toContain('role="radio"');
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain("Aprovar");
    expect(html).not.toContain("<select");
  });
});
