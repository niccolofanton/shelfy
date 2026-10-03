// Static facet-value lists for the Gallery's filter drawer (category, content
// type, AI status — plan §1.2 #13). There is no facet-values endpoint yet:
// P3-04 builds `GET /facets` and P3-21 wires it in. Until then each facet is a
// fixed list, behind its own function here, so swapping one for a live lookup
// later is a one-function change with no call-site churn.
//
// The category/content-type vocabulary mirrors the desktop's web-reference
// catalog (electron/analyzer.ts WEB_INDUSTRY_ENUM / WEB_PURPOSE_ENUM): today
// only web posts carry `aiCategory`/`aiContentType` (social posts leave them
// null), so filtering by either facet only ever narrows to websites — correct
// until P3 generalizes the catalog schema to every platform.

export interface FacetOption {
  value: string;
  // i18n key, resolved against the `filterDrawer` namespace at render
  // (`facetCategory.<value>` / `facetContentType.<value>`); AI status reuses
  // the existing `aiStatus*` scheme already in that namespace isn't needed,
  // its own `facetAiStatus.<value>` key is added instead.
  key: string;
}

export function categoryOptions(): FacetOption[] {
  return [
    { value: 'technology', key: 'technology' },
    { value: 'fintech', key: 'fintech' },
    { value: 'fashion', key: 'fashion' },
    { value: 'food-beverage', key: 'foodBeverage' },
    { value: 'real-estate', key: 'realEstate' },
    { value: 'health', key: 'health' },
    { value: 'education', key: 'education' },
    { value: 'gaming', key: 'gaming' },
    { value: 'travel', key: 'travel' },
    { value: 'b2b-software', key: 'b2bSoftware' },
    { value: 'nonprofit', key: 'nonprofit' },
    { value: 'crypto-web3', key: 'cryptoWeb3' },
    { value: 'architecture', key: 'architecture' },
    { value: 'automotive', key: 'automotive' },
    { value: 'media-entertainment', key: 'mediaEntertainment' },
    { value: 'ecommerce-retail', key: 'ecommerceRetail' },
    { value: 'marketing-agency', key: 'marketingAgency' },
    { value: 'sports', key: 'sports' },
    { value: 'beauty', key: 'beauty' },
    { value: 'other', key: 'other' },
  ];
}

export function contentTypeOptions(): FacetOption[] {
  return [
    { value: 'portfolio', key: 'portfolio' },
    { value: 'e-commerce', key: 'eCommerce' },
    { value: 'saas', key: 'saas' },
    { value: 'landing', key: 'landing' },
    { value: 'agency', key: 'agency' },
    { value: 'editorial', key: 'editorial' },
    { value: 'corporate', key: 'corporate' },
    { value: 'docs', key: 'docs' },
    { value: 'webapp', key: 'webapp' },
    { value: 'directory', key: 'directory' },
    { value: 'personal', key: 'personal' },
    { value: 'other', key: 'other' },
  ];
}

// The desktop's AI analysis lifecycle (types/domain.d.ts Shelfy.AiStatus) —
// a genuinely closed, stable enum, not a stand-in for a future endpoint.
export function aiStatusOptions(): FacetOption[] {
  return [
    { value: 'pending', key: 'pending' },
    { value: 'analyzing', key: 'analyzing' },
    { value: 'done', key: 'done' },
    { value: 'error', key: 'error' },
  ];
}
