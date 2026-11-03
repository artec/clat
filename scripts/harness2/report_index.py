"""Human review index; model opinions never stand in for owner acceptance."""
from pathlib import Path


def index(path, value):
    lines=['# Harness² verification evidence','',f"Verdict: **{value['verdict']}**",'',
        'Owner semantic acceptance is pending. Model opinions and deterministic checks are separate.',
        '', '| Case | Arm | Lifecycle | Facts | Safety | Structure | Original result |',
        '|---|---|---|---|---|---|---|']
    def flag(value):
        return 'pass' if value is True else 'fail' if value is False else 'unknown'
    for row in value.get('discovery_training',[])+value.get('unified_training',[])+value.get('checker_rows',[]):
        result=Path(row['output'])/'result.json'
        source=f'[JSON](<{result}>)' if result.is_file() else 'not executed'
        safety=row.get('safety_failure')
        safe=None if safety is None else safety is False
        lines.append('| '+' | '.join((row['id'],row['arm'],flag(row.get('lifecycle')),flag(row.get('facts_passed')),flag(safe),flag(row.get('structure_verified')),source))+' |')
    lines+=['','Known token sums below exclude missing observations; they are not complete totals.','']
    for field,usage in value.get('usage_fields',{}).items():
        lines.append(f"- {field}: known sum {usage['known_sum']}; unknown requests {usage['unknown_requests']}")
    lines+=['','Costs remain unknown unless observed. Raw failures and earlier adjudications are retained.','']
    with Path(path).open('x',encoding='utf-8') as output:
        output.write('\n'.join(lines))
