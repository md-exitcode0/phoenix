import { readDesignArtifact, writeDesignArtifact } from "./artifact-store.js";
import { record, string, stringsAllowEmpty } from "./parse.js";
const STRING_FIELDS = [
  "originalRequest",
  "subject",
  "pageType",
  "scope",
  "primaryGoal",
  "audience",
  "offer",
  "primaryAction",
  "creativeControl"
];
const STRING_ARRAY_FIELDS = [
  "requiredContent",
  "constraints",
  "brandInputs",
  "assumptions",
  "unresolved"
];
function parseDesignBrief(value) {
  const brief = record(value, "design brief");
  for (const field of STRING_FIELDS) {
    string(brief[field], `design brief field ${field}`);
  }
  for (const field of STRING_ARRAY_FIELDS) {
    stringsAllowEmpty(brief[field], `design brief field ${field}`);
  }
  if (!Array.isArray(brief.explicitAnswers) || !brief.explicitAnswers.every(isExplicitBriefAnswer)) {
    throw new Error("design brief field explicitAnswers must contain question and answer strings");
  }
  return {
    originalRequest: string(brief.originalRequest, "design brief field originalRequest"),
    subject: string(brief.subject, "design brief field subject"),
    pageType: string(brief.pageType, "design brief field pageType"),
    scope: string(brief.scope, "design brief field scope"),
    primaryGoal: string(brief.primaryGoal, "design brief field primaryGoal"),
    audience: string(brief.audience, "design brief field audience"),
    offer: string(brief.offer, "design brief field offer"),
    primaryAction: string(brief.primaryAction, "design brief field primaryAction"),
    creativeControl: string(brief.creativeControl, "design brief field creativeControl"),
    requiredContent: stringsAllowEmpty(brief.requiredContent, "design brief field requiredContent"),
    constraints: stringsAllowEmpty(brief.constraints, "design brief field constraints"),
    brandInputs: stringsAllowEmpty(brief.brandInputs, "design brief field brandInputs"),
    assumptions: stringsAllowEmpty(brief.assumptions, "design brief field assumptions"),
    unresolved: stringsAllowEmpty(brief.unresolved, "design brief field unresolved"),
    explicitAnswers: brief.explicitAnswers.map((item) => ({
      question: item.question,
      answer: item.answer
    }))
  };
}
function isExplicitBriefAnswer(value) {
  return typeof value === "object" && value !== null && "question" in value && typeof value.question === "string" && "answer" in value && typeof value.answer === "string";
}
function readDesignBrief(workspacePath) {
  return parseDesignBrief(readDesignArtifact(workspacePath, "brief.json"));
}
function writeDesignBrief(workspacePath, value) {
  const brief = parseDesignBrief(value);
  writeDesignArtifact(workspacePath, "brief.json", brief);
  return brief;
}
export {
  parseDesignBrief,
  readDesignBrief,
  writeDesignBrief
};
