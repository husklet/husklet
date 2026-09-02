import { families, props, tags } from './catalogue.js';
import { defaults } from './defaults.js';

/** Machine-readable audit of what the shipped selectable catalogue can demonstrate. */
export function storyCoverage() {
  const componentStories = tags.map((tag) => ({
    story: tag.name,
    component: tag.name,
    family: tag.family,
    propertyGroups: [...new Set(props.filter((prop) => tag.props.includes(prop.name)).map((prop) => prop.group))],
    interactions: tag.triggers,
    state: defaults(tag.name),
  }));
  return {
    componentStories,
    families: new Set(componentStories.map((story) => story.family)),
    propertyGroups: new Set(componentStories.flatMap((story) => story.propertyGroups)),
    interactions: new Set(componentStories.flatMap((story) => story.interactions)),
    expectedFamilies: new Set(families.map((family) => family.name)),
    expectedPropertyGroups: new Set(props.map((prop) => prop.group)),
    expectedInteractions: new Set(tags.flatMap((tag) => tag.triggers)),
  };
}
